use std::{
	any::Any,
	io::{Error, ErrorKind, Write},
	os::windows::io::{AsHandle, AsRawHandle},
	process::Command as StdCommand,
	time::Instant,
};

use tokio::io::{AsyncBufReadExt, BufReader};
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCENDANT_MARKER: &str = "PW-JOB-HANDLE-DESCENDANT:";

/// A process opened by ID, so its exit can be observed; terminated if a test fails before it exits.
struct ProcessGuard(HANDLE);

impl ProcessGuard {
	fn open(pid: u32) -> Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(Error::other)?;
		Ok(Self(handle))
	}

	fn has_exited(&self) -> Result<bool> {
		match unsafe { WaitForSingleObject(self.0, 0) } {
			WAIT_OBJECT_0 => Ok(true),
			WAIT_TIMEOUT => Ok(false),
			_ => Err(Error::last_os_error()),
		}
	}

	async fn wait_for_exit(&self) -> Result<()> {
		let deadline = Instant::now() + EXIT_TIMEOUT;
		while !self.has_exited()? {
			if Instant::now() >= deadline {
				return Err(Error::new(ErrorKind::TimedOut, "process did not exit"));
			}
			sleep(Duration::from_millis(10)).await;
		}
		Ok(())
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		unsafe { TerminateProcess(self.0, 1) }.ok();
		unsafe { CloseHandle(self.0) }.ok();
	}
}

/// A process which starts another, and reports its PID.
fn tree() -> CommandWrap {
	CommandWrap::with_new(std::env::current_exe().unwrap(), |command| {
		command
			.args(["job_handle_parent", "--ignored", "--nocapture"])
			.stdout(Stdio::piped());
	})
}

async fn descendant_pid(child: &mut dyn ChildWrapper) -> Result<u32> {
	let stdout = child.stdout().take().expect("the helper's stdout is piped");
	let mut lines = BufReader::new(stdout).lines();
	tokio::time::timeout(STARTUP_TIMEOUT, async {
		while let Some(line) = lines.next_line().await? {
			if let Some(pid) = line.strip_prefix(DESCENDANT_MARKER) {
				return pid.trim().parse().map_err(Error::other);
			}
		}
		Err(Error::other(
			"helper exited before reporting its descendant",
		))
	})
	.await
	.map_err(|_| Error::new(ErrorKind::TimedOut, "helper did not report its descendant"))?
}

fn job_handle(child: &dyn ChildWrapper) -> JobObjectHandle {
	(child as &dyn Any)
		.downcast_ref::<JobObjectChild>()
		.expect("JobObject is the outermost wrapper")
		.job_handle()
}

#[test]
#[ignore = "subprocess helper"]
fn job_handle_leaf() {
	std::thread::sleep(Duration::from_secs(300));
}

#[test]
#[ignore = "subprocess helper"]
fn job_handle_parent() {
	// The leaf's output would go to a pipe the test closes once it has read the PID.
	let mut descendant = StdCommand::new(std::env::current_exe().unwrap())
		.args(["job_handle_leaf", "--ignored"])
		.stdout(Stdio::null())
		.spawn()
		.unwrap();
	println!("{DESCENDANT_MARKER}{}", descendant.id());
	std::io::stdout().flush().unwrap();
	descendant.wait().unwrap();
}

#[tokio::test]
async fn terminates_the_job_from_another_thread_while_the_child_is_awaited() -> Result<()> {
	let mut child = tree().wrap(JobObject).spawn()?;
	let leader = ProcessGuard::open(child.id().expect("the helper is running"))?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut()).await?)?;

	let job = job_handle(child.as_ref());
	assert_eq!(
		job.as_handle().as_raw_handle(),
		job_handle(child.as_ref()).as_handle().as_raw_handle(),
		"every job handle shares the one job handle"
	);

	let terminator = std::thread::spawn(move || job.terminate(7));
	let status = tokio::time::timeout(EXIT_TIMEOUT, child.wait())
		.await
		.map_err(|_| Error::new(ErrorKind::TimedOut, "child did not exit"))??;
	terminator
		.join()
		.expect("the terminating thread did not panic")?;

	assert_eq!(status.code(), Some(7));
	leader.wait_for_exit().await?;
	descendant.wait_for_exit().await?;
	Ok(())
}

#[cfg(feature = "kill-on-drop")]
#[tokio::test]
async fn keeps_a_kill_on_drop_job_open_after_the_child_is_dropped() -> Result<()> {
	let mut child = tree().wrap(JobObject).wrap(KillOnDrop).spawn()?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut()).await?)?;
	let job = job_handle(child.as_ref());

	drop(child);
	sleep(DIE_TIME).await;
	assert!(
		!descendant.has_exited()?,
		"descendant outlived the child while the job handle was held"
	);

	drop(job);
	descendant.wait_for_exit().await?;
	Ok(())
}
