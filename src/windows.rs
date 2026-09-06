//! Windows API support functions.

use std::{
	io::{Error, Result},
	ops::ControlFlow,
	time::{Duration, Instant},
};

#[cfg(feature = "tracing")]
use tracing::{debug, instrument};
use windows::{
	Win32::{
		Foundation::{
			CloseHandle, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
		},
		System::{
			Diagnostics::ToolHelp::{
				CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
				Thread32Next,
			},
			IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED},
			JobObjects::{
				AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
				JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
				JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
				JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
				QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
			},
			Threading::{
				CREATE_SUSPENDED, GetProcessId, OpenThread, PROCESS_CREATION_FLAGS, ResumeThread,
				THREAD_SUSPEND_RESUME,
			},
		},
	},
	core::HRESULT,
};

#[derive(Debug)]
struct OwnedHandle(HANDLE);

impl OwnedHandle {
	fn into_raw(self) -> HANDLE {
		let handle = self.0;
		std::mem::forget(self);
		handle
	}
}

impl Drop for OwnedHandle {
	fn drop(&mut self) {
		unsafe { CloseHandle(self.0) }.ok();
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JobCreationFlags {
	pub flags: PROCESS_CREATION_FLAGS,
	pub resume_after_assignment: bool,
}

pub(crate) fn job_creation_flags(user_flags: PROCESS_CREATION_FLAGS) -> JobCreationFlags {
	JobCreationFlags {
		flags: user_flags | CREATE_SUSPENDED,
		resume_after_assignment: !user_flags.contains(CREATE_SUSPENDED),
	}
}

#[cfg(test)]
mod creation_flag_tests {
	use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

	use super::*;

	#[test]
	fn adds_suspension_without_losing_user_flags() {
		let user_flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
		let policy = job_creation_flags(user_flags);
		assert_eq!(policy.flags, user_flags | CREATE_SUSPENDED);
		assert!(policy.resume_after_assignment);
	}

	#[test]
	fn preserves_explicit_suspension() {
		let user_flags = CREATE_NO_WINDOW | CREATE_SUSPENDED;
		let policy = job_creation_flags(user_flags);
		assert_eq!(policy.flags, user_flags);
		assert!(!policy.resume_after_assignment);
	}
}

#[derive(Clone, Copy, Debug)]
pub struct JobHandle(pub HANDLE);

unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

#[derive(Clone, Copy, Debug)]
pub struct PortHandle(pub HANDLE);

unsafe impl Send for PortHandle {}
unsafe impl Sync for PortHandle {}

/// A JobObject and its associated completion port.
///
/// This struct closes the handles when dropped.
#[derive(Debug)]
pub(crate) struct JobPort {
	pub job: JobHandle,
	pub completion_port: PortHandle,
}

#[cfg(feature = "tokio1")]
impl JobPort {
	pub(crate) fn try_clone(&self) -> Result<Self> {
		use std::os::windows::io::{BorrowedHandle, IntoRawHandle};
		let job = unsafe { BorrowedHandle::borrow_raw(self.job.0.0) }.try_clone_to_owned()?;
		let port =
			unsafe { BorrowedHandle::borrow_raw(self.completion_port.0.0) }.try_clone_to_owned()?;
		Ok(Self {
			job: JobHandle(HANDLE(job.into_raw_handle())),
			completion_port: PortHandle(HANDLE(port.into_raw_handle())),
		})
	}
}

impl Drop for JobPort {
	fn drop(&mut self) {
		unsafe { CloseHandle(self.job.0) }.ok();
		unsafe { CloseHandle(self.completion_port.0) }.ok();
	}
}

/// Create a JobObject and an associated completion port.
///
/// If `kill_on_drop` is true, we opt into the `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag, which
/// essentially implements the "reap children" feature of Unix systems directly in Win32.
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn make_job_object(process_handle: HANDLE, kill_on_drop: bool) -> Result<JobPort> {
	let job = OwnedHandle(unsafe { CreateJobObjectW(None, None) }.map_err(Error::other)?);
	#[cfg(feature = "tracing")]
	debug!(?job, "done CreateJobObjectW");

	let completion_port =
		OwnedHandle(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }?);
	#[cfg(feature = "tracing")]
	debug!(?completion_port, "done CreateIoCompletionPort");

	let associate_completion = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
		CompletionKey: job.0.0 as _,
		CompletionPort: completion_port.0,
	};

	unsafe {
		SetInformationJobObject(
			job.0,
			JobObjectAssociateCompletionPortInformation,
			(&associate_completion) as *const _ as _,
			std::mem::size_of_val(&associate_completion)
				.try_into()
				.expect("cannot safely cast to DWORD"),
		)
	}?;
	#[cfg(feature = "tracing")]
	debug!(
		?associate_completion,
		"done SetInformationJobObject(completion)"
	);

	let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();

	if kill_on_drop {
		info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
	}

	unsafe {
		SetInformationJobObject(
			job.0,
			JobObjectExtendedLimitInformation,
			&info as *const _ as _,
			std::mem::size_of_val(&info)
				.try_into()
				.expect("cannot safely cast to DWORD"),
		)
	}?;
	#[cfg(feature = "tracing")]
	debug!(?info, "done SetInformationJobObject(limit)");

	unsafe { AssignProcessToJobObject(job.0, process_handle) }?;
	#[cfg(feature = "tracing")]
	debug!(?job, ?process_handle, "done AssignProcessToJobObject");

	Ok(JobPort {
		job: JobHandle(job.into_raw()),
		completion_port: PortHandle(completion_port.into_raw()),
	})
}

/// Resume all threads in the process (ie resume the process).
///
/// This is a pretty terrible hack, but it's either this or we
/// re-implement all of Rust's std::process just to get access!
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn resume_threads(child_process: HANDLE) -> Result<()> {
	#[inline]
	unsafe fn inner(pid: u32, tool_handle: HANDLE) -> Result<()> {
		let mut entry = THREADENTRY32 {
			dwSize: std::mem::size_of::<THREADENTRY32>()
				.try_into()
				.expect("THREADENTRY32 is guaranteed to fit in a DWORD"),
			..Default::default()
		};
		unsafe { Thread32First(tool_handle, &mut entry) }.map_err(Error::other)?;

		let mut resumed = false;
		loop {
			if entry.th32OwnerProcessID == pid {
				let thread_handle = OwnedHandle(unsafe {
					OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
				}?);
				let previous_count = unsafe { ResumeThread(thread_handle.0) };
				if previous_count == u32::MAX {
					return Err(Error::last_os_error());
				}
				if previous_count > 0 {
					resumed = true;
				}
			}

			match unsafe { Thread32Next(tool_handle, &mut entry) } {
				Ok(()) => {}
				Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => {
					break;
				}
				Err(error) => return Err(Error::other(error)),
			}
		}

		if resumed {
			Ok(())
		} else {
			Err(Error::other("no thread belonging to the child was found"))
		}
	}

	let child_id = unsafe { GetProcessId(child_process) };
	if child_id == 0 {
		return Err(Error::last_os_error());
	}

	let tool_handle = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	unsafe { inner(child_id, tool_handle.0) }
}

/// Terminate a job object without waiting for the processes to exit.
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn terminate_job(job: JobHandle, exit_code: u32) -> Result<()> {
	unsafe { TerminateJobObject(job.0, exit_code) }.map_err(Error::other)
}

/// Wait for a job to complete.
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn wait_on_job(
	job: JobHandle,
	completion_port: PortHandle,
	timeout: Option<Duration>,
) -> Result<ControlFlow<()>> {
	let started = Instant::now();
	loop {
		let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
		unsafe {
			QueryInformationJobObject(
				Some(job.0),
				JobObjectBasicAccountingInformation,
				&mut accounting as *mut _ as _,
				std::mem::size_of_val(&accounting)
					.try_into()
					.expect("accounting information fits in a DWORD"),
				None,
			)
		}?;
		if accounting.ActiveProcesses == 0 {
			return Ok(ControlFlow::Break(()));
		}
		let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
		if remaining == Some(Duration::ZERO) {
			return Ok(ControlFlow::Continue(()));
		}
		// Job notifications can be unrelated or lost; accounting is the completion oracle.
		let interval = remaining
			.unwrap_or(Duration::from_millis(10))
			.min(Duration::from_millis(10));
		let mut code = 0;
		let mut key = 0;
		let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
		let result = unsafe {
			GetQueuedCompletionStatus(
				completion_port.0,
				&mut code,
				&mut key,
				&mut overlapped,
				interval.as_millis().max(1) as u32,
			)
		};
		match result {
			Ok(()) => {}
			Err(error) if error.code() == HRESULT::from_win32(WAIT_TIMEOUT.0) => {}
			Err(error) => return Err(Error::other(error)),
		}
	}
}

#[cfg(test)]
mod wait_tests {
	use super::*;

	#[test]
	fn invalid_job_is_an_error_even_for_nonblocking_wait() {
		let job = JobHandle(INVALID_HANDLE_VALUE);
		let port = PortHandle(INVALID_HANDLE_VALUE);
		assert!(wait_on_job(job, port, Some(Duration::ZERO)).is_err());
	}
}
