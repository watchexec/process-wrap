use std::{fs::read_to_string, io::ErrorKind, time::Instant};

use tempfile::NamedTempFile;
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

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

async fn descendant_pid(file: &NamedTempFile) -> Result<u32> {
	let deadline = Instant::now() + STARTUP_TIMEOUT;
	loop {
		match read_to_string(file.path()) {
			Ok(contents) if !contents.trim().is_empty() => {
				return contents.trim().parse().map_err(std::io::Error::other);
			}
			Ok(_) => {}
			Err(error) if error.kind() == ErrorKind::NotFound => {}
			Err(error) => return Err(error),
		}
		if Instant::now() >= deadline {
			return Err(std::io::Error::new(
				ErrorKind::TimedOut,
				"descendant pid was not written",
			));
		}
		sleep(Duration::from_millis(10)).await;
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

#[tokio::test]
async fn job_detects_kill_on_drop_in_both_orders() -> Result<()> {
	for order in [Order::KillOnDropFirst, Order::JobObjectFirst] {
		let pid_file = NamedTempFile::new()?;
		let path = pid_file.path().display().to_string().replace('\'', "''");
		let script = format!(
			"$child = Start-Process powershell.exe -ArgumentList @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 300') -PassThru; Set-Content -LiteralPath '{path}' -Value $child.Id -NoNewline; Start-Sleep -Seconds 300"
		);
		let mut command = CommandWrap::with_new("powershell.exe", |command| {
			command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
		});
		match order {
			Order::KillOnDropFirst => {
				command.wrap(KillOnDrop).wrap(JobObject);
			}
			Order::JobObjectFirst => {
				command.wrap(JobObject).wrap(KillOnDrop);
			}
		}

		let child = command.spawn()?;
		let pid = descendant_pid(&pid_file).await?;
		let guard = ProcessGuard::open(pid)?;
		drop(child);
		wait_for_process_exit(guard.handle()).await?;
		guard.disarm()?;
	}
	Ok(())
}
