//! Windows API support functions.

use std::{
	io::{Error, Result},
	ops::ControlFlow,
	os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle as StdOwnedHandle},
	time::Duration,
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
		// SAFETY: this wrapper solely owns a successfully created handle.
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

#[cfg(test)]
mod job_wait_tests {
	use std::os::windows::io::AsHandle;

	use windows::Win32::System::IO::{CreateIoCompletionPort, PostQueuedCompletionStatus};

	use super::*;

	const JOB_OBJECT_MSG_NEW_PROCESS: u32 = 6;

	#[test]
	fn nonterminal_completion_packet_does_not_report_job_drain() {
		// SAFETY: these arguments create a new completion port which is immediately owned below.
		let raw = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }.unwrap();
		// SAFETY: `raw` is a newly created, uniquely owned completion-port handle.
		let port = unsafe { StdOwnedHandle::from_raw_handle(raw.0) };
		// SAFETY: `port` owns a live completion port; the scalar payload does not borrow memory.
		unsafe {
			PostQueuedCompletionStatus(
				HANDLE(port.as_raw_handle()),
				JOB_OBJECT_MSG_NEW_PROCESS,
				0,
				None,
			)
		}
		.unwrap();

		assert_eq!(
			poll_job_drain_with(port.as_handle(), Duration::ZERO, || Ok(false)).unwrap(),
			ControlFlow::Continue(()),
			"a new-process notification is only a wake hint, not proof that the job drained"
		);
	}
}

#[derive(Clone, Copy, Debug)]
pub struct JobHandle(pub HANDLE);

// SAFETY: this non-owning wrapper contains only a process-wide kernel handle value.
unsafe impl Send for JobHandle {}
// SAFETY: shared access exposes only the handle value, not mutable Rust memory.
unsafe impl Sync for JobHandle {}

/// A JobObject and its associated completion port.
///
/// This struct closes the handles when dropped.
#[derive(Debug)]
pub(crate) struct JobPort {
	pub job: JobHandle,
	pub completion_port: StdOwnedHandle,
}

impl Drop for JobPort {
	fn drop(&mut self) {
		// SAFETY: `JobPort` solely owns this job handle.
		unsafe { CloseHandle(self.job.0) }.ok();
	}
}

/// Set whether closing a job's final handle terminates every process in the job.
pub(crate) fn set_job_kill_on_drop(job: JobHandle, kill_on_drop: bool) -> Result<()> {
	let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
	if kill_on_drop {
		info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
	}

	#[cfg(feature = "tracing")]
	debug!(
		kill_on_drop,
		?info,
		"setting SetInformationJobObject(limit)"
	);
	// No tracing or other caller-controlled callback may run after the native transition: the sole
	// final owner must either remain kill-on-close armed or complete disarming without unwinding.
	// SAFETY: `job` is live, and initialized `info` has the reported size and outlives the call.
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
	Ok(())
}

/// Create a JobObject and an associated completion port.
///
/// If `kill_on_drop` is true, we opt into the `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag, which
/// essentially implements the "reap children" feature of Unix systems directly in Win32.
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn make_job_object(process_handle: HANDLE, kill_on_drop: bool) -> Result<JobPort> {
	// SAFETY: null attributes and name request defaults; the successful handle is immediately owned.
	let job = OwnedHandle(unsafe { CreateJobObjectW(None, None) }.map_err(Error::other)?);
	#[cfg(feature = "tracing")]
	debug!(?job, "done CreateJobObjectW");

	// SAFETY: these arguments create a new port; the successful handle is immediately owned.
	let completion_port = unsafe {
		StdOwnedHandle::from_raw_handle(CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1)?.0)
	};
	#[cfg(feature = "tracing")]
	debug!(?completion_port, "done CreateIoCompletionPort");

	let associate_completion = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
		CompletionKey: job.0.0 as _,
		CompletionPort: HANDLE(completion_port.as_raw_handle()),
	};

	// SAFETY: the handles stay live, and initialized `associate_completion` has the reported size.
	// `CompletionKey` is opaque and is not dereferenced.
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

	set_job_kill_on_drop(JobHandle(job.0), kill_on_drop)?;

	// SAFETY: `job` is owned here and `process_handle` is borrowed from the live child.
	unsafe { AssignProcessToJobObject(job.0, process_handle) }?;
	#[cfg(feature = "tracing")]
	debug!(?job, ?process_handle, "done AssignProcessToJobObject");

	Ok(JobPort {
		job: JobHandle(job.into_raw()),
		completion_port,
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
		// SAFETY: `tool_handle` is live; `entry` is writable and has the required `dwSize`.
		unsafe { Thread32First(tool_handle, &mut entry) }.map_err(Error::other)?;

		let mut resumed = false;
		loop {
			if entry.th32OwnerProcessID == pid {
				// SAFETY: snapshot enumeration initialized this thread ID; success is immediately owned.
				let thread_handle = OwnedHandle(unsafe {
					OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
				}?);
				// SAFETY: `thread_handle` owns a live handle with suspend/resume access.
				let previous_count = unsafe { ResumeThread(thread_handle.0) };
				if previous_count == u32::MAX {
					return Err(Error::last_os_error());
				}
				if previous_count > 0 {
					resumed = true;
				}
			}

			// SAFETY: `tool_handle` stays live; `entry` remains writable with the required `dwSize`.
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

	// SAFETY: `child_process` is borrowed from the live child for this call.
	let child_id = unsafe { GetProcessId(child_process) };
	if child_id == 0 {
		return Err(Error::last_os_error());
	}

	// SAFETY: success returns a snapshot handle that is immediately owned.
	let tool_handle = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	// SAFETY: `tool_handle` remains owned and live through `inner`.
	unsafe { inner(child_id, tool_handle.0) }
}

/// Terminate a job object without waiting for the processes to exit.
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn terminate_job(job: JobHandle, exit_code: u32) -> Result<()> {
	// SAFETY: `job` is borrowed from a live `JobPort`; the call takes only scalar values.
	unsafe { TerminateJobObject(job.0, exit_code) }.map_err(Error::other)
}

/// Maximum interval between authoritative JobObject accounting checks.
pub(crate) const JOB_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn job_is_drained(job: JobHandle) -> Result<bool> {
	let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
	// SAFETY: `job` is borrowed from a live `JobPort`; the information class requires exactly the
	// writable accounting structure and byte count supplied here. The call returns before that stack
	// storage is read or dropped.
	unsafe {
		QueryInformationJobObject(
			Some(job.0),
			JobObjectBasicAccountingInformation,
			&mut accounting as *mut _ as _,
			std::mem::size_of_val(&accounting)
				.try_into()
				.expect("job accounting information cannot exceed a DWORD"),
			None,
		)
	}
	.map_err(Error::other)?;
	Ok(accounting.ActiveProcesses == 0)
}

fn poll_job_drain_with(
	completion_port: BorrowedHandle<'_>,
	timeout: Duration,
	mut is_drained: impl FnMut() -> Result<bool>,
) -> Result<ControlFlow<()>> {
	if is_drained()? {
		return Ok(ControlFlow::Break(()));
	}

	let mut code = 0;
	let mut key = 0;
	let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
	let timeout_ms = timeout.as_millis().try_into().unwrap_or(u32::MAX - 1);

	// SAFETY: `completion_port` is lifetime-bound to a live owned handle, and every output pointer
	// refers to initialized writable stack storage for the duration of the call. The finite timeout
	// cannot equal `INFINITE`. A dequeued packet is only a wake hint; none of its scalar fields is
	// treated as proof that the job drained.
	let wake = unsafe {
		GetQueuedCompletionStatus(
			HANDLE(completion_port.as_raw_handle()),
			&mut code,
			&mut key,
			&mut overlapped,
			timeout_ms,
		)
	};
	if let Err(error) = wake
		&& overlapped.is_null()
		&& error.code() != HRESULT::from_win32(WAIT_TIMEOUT.0)
	{
		return Err(Error::other(error));
	}

	if is_drained()? {
		Ok(ControlFlow::Break(()))
	} else {
		Ok(ControlFlow::Continue(()))
	}
}

/// Poll whether a job has no active processes, waiting at most `timeout` for a wake hint.
#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
pub(crate) fn poll_job_drain(
	job: JobHandle,
	completion_port: BorrowedHandle<'_>,
	timeout: Duration,
) -> Result<ControlFlow<()>> {
	poll_job_drain_with(completion_port, timeout, || job_is_drained(job))
}
