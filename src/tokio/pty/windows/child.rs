//! A terminal [`ChildWrapper`] backed directly by Win32 process handles.

use std::{
	future::Future,
	io,
	os::windows::{
		io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
		process::ExitStatusExt,
	},
	pin::Pin,
	process::ExitStatus,
};

use tokio::{
	process::{ChildStderr, ChildStdin, ChildStdout},
	task::spawn_blocking,
};
#[cfg(feature = "job-object")]
use windows::Win32::System::Threading::ResumeThread;
use windows::Win32::{
	Foundation::{
		DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
	},
	System::Threading::{
		GetCurrentProcess, GetExitCodeProcess, INFINITE, TerminateProcess, WaitForSingleObject,
	},
};

use crate::{ChildExitStatus, tokio::ChildWrapper};

#[derive(Debug)]
pub(super) struct ConPtyChild {
	process: OwnedHandle,
	#[cfg_attr(not(feature = "job-object"), allow(dead_code))]
	primary_thread: Option<OwnedHandle>,
	pid: u32,
	kill_on_drop: bool,
	exit_status: ChildExitStatus,
	stdin: Option<ChildStdin>,
	stdout: Option<ChildStdout>,
	stderr: Option<ChildStderr>,
}

impl ConPtyChild {
	pub(super) fn new(
		process: OwnedHandle,
		primary_thread: OwnedHandle,
		pid: u32,
		kill_on_drop: bool,
	) -> Self {
		Self {
			process,
			primary_thread: Some(primary_thread),
			pid,
			kill_on_drop,
			exit_status: ChildExitStatus::Running,
			stdin: None,
			stdout: None,
			stderr: None,
		}
	}

	#[cfg(test)]
	pub(super) fn primary_thread_handle(&self) -> Option<BorrowedHandle<'_>> {
		self.primary_thread.as_ref().map(OwnedHandle::as_handle)
	}

	#[cfg(feature = "job-object")]
	pub(super) fn resume_primary_thread(&mut self) -> io::Result<()> {
		let thread = self
			.primary_thread
			.take()
			.ok_or_else(|| io::Error::other("ConPTY child has no primary thread handle"))?;
		// SAFETY: thread is the live primary-thread handle returned by CreateProcessW.
		let previous_count = unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) };
		match previous_count {
			u32::MAX => Err(io::Error::last_os_error()),
			1 => Ok(()),
			0 => Err(io::Error::other(
				"ConPTY primary thread was not suspended for job assignment",
			)),
			count => Err(io::Error::other(format!(
				"ConPTY primary thread remains suspended with count {}",
				count - 1
			))),
		}
	}

	fn raw_process_handle(&self) -> HANDLE {
		HANDLE(self.process.as_raw_handle())
	}
}

impl ChildWrapper for ConPtyChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		Some(self.process.as_handle())
	}

	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		&mut self.stdin
	}

	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		&mut self.stdout
	}

	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		&mut self.stderr
	}

	fn id(&self) -> Option<u32> {
		Some(self.pid)
	}

	fn start_kill(&mut self) -> io::Result<()> {
		if self.try_wait()?.is_some() {
			return Ok(());
		}
		// SAFETY: self owns a live process handle with process-termination access.
		unsafe { TerminateProcess(self.raw_process_handle(), 1) }.map_err(io::Error::other)
	}

	fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
		if let ChildExitStatus::Exited(status) = self.exit_status {
			return Ok(Some(status));
		}
		let status = process_status(self.raw_process_handle(), 0)?;
		if let Some(status) = status {
			self.exit_status = ChildExitStatus::Exited(status);
		}
		Ok(status)
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
		Box::pin(async move {
			if let ChildExitStatus::Exited(status) = self.exit_status {
				return Ok(status);
			}

			let process = duplicate_handle(&self.process)?;
			let status = spawn_blocking(move || {
				process_status(HANDLE(process.as_raw_handle()), INFINITE)?.ok_or_else(|| {
					io::Error::other("infinite process wait returned without an exit status")
				})
			})
			.await
			.map_err(io::Error::other)??;
			self.exit_status = ChildExitStatus::Exited(status);
			Ok(status)
		})
	}
}

impl Drop for ConPtyChild {
	fn drop(&mut self) {
		if self.kill_on_drop
			&& matches!(self.exit_status, ChildExitStatus::Running)
			&& !matches!(process_status(self.raw_process_handle(), 0), Ok(Some(_)))
		{
			// SAFETY: self still owns the process handle for the duration of this call.
			let _ = unsafe { TerminateProcess(self.raw_process_handle(), 1) };
		}
	}
}

fn duplicate_handle(handle: &OwnedHandle) -> io::Result<OwnedHandle> {
	// SAFETY: GetCurrentProcess returns a permanent pseudo-handle. The source handle is live, and the
	// output pointer is writable. The resulting non-inheritable duplicate is transferred once.
	unsafe {
		let process = GetCurrentProcess();
		let mut duplicate = HANDLE::default();
		DuplicateHandle(
			process,
			HANDLE(handle.as_raw_handle()),
			process,
			&mut duplicate,
			0,
			false,
			DUPLICATE_SAME_ACCESS,
		)
		.map_err(io::Error::other)?;
		Ok(OwnedHandle::from_raw_handle(duplicate.0))
	}
}

fn process_status(process: HANDLE, timeout: u32) -> io::Result<Option<ExitStatus>> {
	// SAFETY: process is a live process handle and timeout has the documented wait semantics.
	match unsafe { WaitForSingleObject(process, timeout) } {
		WAIT_TIMEOUT => Ok(None),
		WAIT_OBJECT_0 => {
			let mut code = 0;
			// SAFETY: the process is signaled and code points to initialized writable storage.
			unsafe { GetExitCodeProcess(process, &mut code) }.map_err(io::Error::other)?;
			Ok(Some(ExitStatus::from_raw(code)))
		}
		WAIT_FAILED => Err(io::Error::last_os_error()),
		other => Err(io::Error::other(format!(
			"unexpected process wait result {}",
			other.0
		))),
	}
}

#[cfg(test)]
mod tests {
	use std::{
		os::windows::io::{AsRawHandle, FromRawHandle},
		process::{Child, Command},
		thread::sleep,
		time::{Duration, Instant},
	};

	use super::*;

	fn duplicate_process(child: &Child) -> OwnedHandle {
		// SAFETY: the current-process pseudo-handle and child's process handle are live, and duplicate
		// points to initialized writable storage. Ownership of the result is transferred once.
		unsafe {
			let current = GetCurrentProcess();
			let mut duplicate = HANDLE::default();
			DuplicateHandle(
				current,
				HANDLE(child.as_raw_handle()),
				current,
				&mut duplicate,
				0,
				false,
				DUPLICATE_SAME_ACCESS,
			)
			.unwrap();
			OwnedHandle::from_raw_handle(duplicate.0)
		}
	}

	fn wrap(child: &Child, kill_on_drop: bool) -> ConPtyChild {
		ConPtyChild {
			process: duplicate_process(child),
			primary_thread: None,
			pid: child.id(),
			kill_on_drop,
			exit_status: ChildExitStatus::Running,
			stdin: None,
			stdout: None,
			stderr: None,
		}
	}

	fn spawn_exit_259() -> Child {
		Command::new("cmd.exe")
			.args(["/D", "/C", "exit /b 259"])
			.spawn()
			.unwrap()
	}

	fn spawn_long_running() -> Child {
		Command::new("ping.exe")
			.args(["-n", "30", "127.0.0.1"])
			.spawn()
			.unwrap()
	}

	fn wait_native(child: &mut Child) -> ExitStatus {
		let deadline = Instant::now() + Duration::from_secs(5);
		loop {
			if let Some(status) = child.try_wait().unwrap() {
				return status;
			}
			assert!(Instant::now() < deadline, "native test child did not exit");
			sleep(Duration::from_millis(10));
		}
	}

	#[tokio::test]
	async fn preserves_exit_code_259_and_repeated_waits() {
		let mut native = spawn_exit_259();
		let mut child = wrap(&native, false);
		assert_eq!(child.id(), Some(native.id()));
		assert!(child.process_handle().is_some());
		assert!(child.primary_thread_handle().is_none());
		assert!((&child as &dyn ChildWrapper).try_inner_child().is_none());

		let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(status.code(), Some(259));
		assert_eq!(child.try_wait().unwrap(), Some(status));
		assert_eq!(child.wait().await.unwrap(), status);
		assert_eq!(wait_native(&mut native), status);
	}

	#[tokio::test]
	async fn kills_and_reaps_the_direct_process() {
		let mut native = spawn_long_running();
		let mut child = wrap(&native, false);
		assert!(child.try_wait().unwrap().is_none());
		child.start_kill().unwrap();
		let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(child.try_wait().unwrap(), Some(status));
		assert_eq!(wait_native(&mut native), status);
	}

	#[tokio::test]
	async fn canceled_async_wait_does_not_invalidate_the_process_handle() {
		let mut native = spawn_long_running();
		let mut child = wrap(&native, false);
		assert!(
			tokio::time::timeout(Duration::from_millis(20), child.wait())
				.await
				.is_err()
		);
		child.start_kill().unwrap();
		let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(wait_native(&mut native), status);
	}

	#[test]
	fn kill_on_drop_terminates_a_running_process() {
		let mut native = spawn_long_running();
		let child = wrap(&native, true);
		drop(child);
		let _ = wait_native(&mut native);
	}
}
