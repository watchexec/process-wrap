use std::{
	io::{ErrorKind, Write},
	process::Command as StdCommand,
	time::Instant,
};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCENDANT_MARKER: &str = "PW-DESCENDANT:";

#[derive(Clone, Copy)]
enum Order {
	KillOnDropFirst,
	JobObjectFirst,
}

struct ProcessGuard(Option<HANDLE>);

impl ProcessGuard {
	fn open(pid: u32) -> Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(std::io::Error::other)?;
		Ok(Self(Some(handle)))
	}

	fn handle(&self) -> HANDLE {
		self.0.expect("the process guard must still be armed")
	}

	fn disarm(mut self) -> Result<()> {
		if let Some(handle) = self.0.take() {
			unsafe { CloseHandle(handle) }?;
		}
		Ok(())
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		if let Some(handle) = self.0.take() {
			unsafe { TerminateProcess(handle, 1) }.ok();
			unsafe { CloseHandle(handle) }.ok();
		}
	}
}

async fn descendant_pid(child: &mut dyn ChildWrapper) -> Result<u32> {
	let stdout = child.stdout().take().ok_or_else(|| {
		std::io::Error::new(
			ErrorKind::BrokenPipe,
			"descendant helper stdout was not piped",
		)
	})?;
	let mut lines = BufReader::new(stdout).lines();
	let result = tokio::time::timeout(STARTUP_TIMEOUT, async {
		while let Some(line) = lines.next_line().await? {
			if let Some(pid) = line.strip_prefix(DESCENDANT_MARKER) {
				return pid.trim().parse().map_err(std::io::Error::other);
			}
		}

		let status = child.wait().await?;
		let mut stderr = String::new();
		if let Some(mut pipe) = child.stderr().take() {
			pipe.read_to_string(&mut stderr).await?;
		}
		Err(std::io::Error::other(format!(
			"descendant helper exited with {status} before reporting its pid: {stderr}"
		)))
	})
	.await;

	match result {
		Ok(result) => result,
		Err(_) => Err(std::io::Error::new(
			ErrorKind::TimedOut,
			format!(
				"descendant helper did not report its pid; process status: {:?}",
				child.try_wait()?
			),
		)),
	}
}

async fn wait_for_process_exit(handle: HANDLE) -> Result<()> {
	let deadline = Instant::now() + EXIT_TIMEOUT;
	loop {
		let wait = unsafe { WaitForSingleObject(handle, 0) };
		if wait == WAIT_OBJECT_0 {
			return Ok(());
		}
		if wait == WAIT_FAILED {
			return Err(std::io::Error::last_os_error());
		}
		if Instant::now() >= deadline {
			return Err(std::io::Error::new(
				ErrorKind::TimedOut,
				"descendant survived dropping the job",
			));
		}
		sleep(Duration::from_millis(10)).await;
	}
}

#[test]
#[ignore = "subprocess helper"]
fn descendant_leaf() {
	std::thread::sleep(Duration::from_secs(300));
}

#[test]
#[ignore = "subprocess helper"]
fn descendant_parent() {
	let mut descendant = StdCommand::new(std::env::current_exe().unwrap())
		.args([
			"--ignored",
			"--exact",
			concat!(module_path!(), "::descendant_leaf"),
			"--nocapture",
		])
		.spawn()
		.unwrap();
	println!("{DESCENDANT_MARKER}{}", descendant.id());
	std::io::stdout().flush().unwrap();
	descendant.wait().unwrap();
}

#[tokio::test]
async fn job_detects_kill_on_drop_in_both_orders() -> Result<()> {
	for order in [Order::KillOnDropFirst, Order::JobObjectFirst] {
		let mut command = CommandWrap::with_new(std::env::current_exe()?, |command| {
			command
				.args([
					"--ignored",
					"--exact",
					concat!(module_path!(), "::descendant_parent"),
					"--nocapture",
				])
				.stdout(Stdio::piped())
				.stderr(Stdio::piped());
		});
		match order {
			Order::KillOnDropFirst => {
				command.wrap(KillOnDrop).wrap(JobObject);
			}
			Order::JobObjectFirst => {
				command.wrap(JobObject).wrap(KillOnDrop);
			}
		}

		let mut child = command.spawn()?;
		let pid = descendant_pid(child.as_mut()).await?;
		let guard = ProcessGuard::open(pid)?;
		drop(child);
		wait_for_process_exit(guard.handle()).await?;
		guard.disarm()?;
	}
	Ok(())
}
