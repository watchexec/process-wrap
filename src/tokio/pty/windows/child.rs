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

	pub(super) fn primary_thread_handle(&self) -> Option<BorrowedHandle<'_>> {
		self.primary_thread.as_ref().map(OwnedHandle::as_handle)
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
