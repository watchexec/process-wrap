//! Windows API support functions.

use std::{
	io::{Error, Result},
	ops::ControlFlow,
	os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle as ProcessHandle},
	sync::{Arc, Mutex, TryLockError},
	time::{Duration, Instant},
};

#[cfg(feature = "tracing")]
use tracing::{debug, instrument};
use windows::{
	Win32::{
		Foundation::{
			CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, HANDLE,
			INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
		},
		System::{
			Diagnostics::ToolHelp::{
				CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
				Thread32Next,
			},
			IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED},
			JobObjects::{
				AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
				JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
				JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
				JobObjectAssociateCompletionPortInformation, JobObjectBasicAccountingInformation,
				JobObjectExtendedLimitInformation, QueryInformationJobObject,
				SetInformationJobObject, TerminateJobObject,
			},
			SystemServices::JOB_OBJECT_MSG_NEW_PROCESS,
			Threading::{
				CREATE_SUSPENDED, GetProcessId, OpenProcess, OpenThread, PROCESS_CREATION_FLAGS,
				PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ResumeThread,
				THREAD_SUSPEND_RESUME, WaitForSingleObject,
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
	completion: Arc<Mutex<JobCompletion>>,
}

impl JobPort {
	pub(crate) fn detach(mut self) {
		// into_inner preserves the original job lifetime while releasing observation resources.
		self.job = JobHandle(INVALID_HANDLE_VALUE);
	}
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
			completion: Arc::clone(&self.completion),
		})
	}
}

impl Drop for JobPort {
	fn drop(&mut self) {
		if self.job.0 != INVALID_HANDLE_VALUE {
			unsafe { CloseHandle(self.job.0) }.ok();
		}
		if self.completion_port.0 != INVALID_HANDLE_VALUE {
			unsafe { CloseHandle(self.completion_port.0) }.ok();
		}
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

	let completion = Arc::new(Mutex::new(JobCompletion {
		key: job.0.0 as usize,
		observed: 0,
		pending: Vec::new(),
	}));
	Ok(JobPort {
		completion,
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

#[derive(Debug, Default)]
struct JobCompletion {
	key: usize,
	observed: u32,
	pending: Vec<ProcessHandle>,
}

impl JobCompletion {
	fn observe_process(&mut self, job: JobHandle, pid: usize) -> Result<()> {
		let observed = self
			.observed
			.checked_add(1)
			.ok_or_else(|| Error::other("job process census overflow"))?;
		let pid = u32::try_from(pid).map_err(Error::other)?;
		let handle = match unsafe {
			OpenProcess(
				PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
				false,
				pid,
			)
		} {
			Ok(handle) => unsafe { ProcessHandle::from_raw_handle(handle.0) },
			Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
				self.observed = observed;
				return Ok(());
			}
			Err(error) => return Err(Error::other(error)),
		};
		let mut belongs = Default::default();
		unsafe { IsProcessInJob(HANDLE(handle.as_raw_handle()), Some(job.0), &mut belongs) }?;
		if belongs.as_bool() {
			self.pending.push(handle);
		}
		self.observed = observed;
		Ok(())
	}

	fn poll(&mut self, job: JobHandle, port: PortHandle) -> Result<ControlFlow<()>> {
		let mut drained = false;
		for _ in 0..64 {
			let mut code = 0;
			let mut key = 0;
			let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
			match unsafe {
				GetQueuedCompletionStatus(port.0, &mut code, &mut key, &mut overlapped, 0)
			} {
				Ok(()) => {
					if key != self.key {
						return Err(Error::other("unexpected job completion key"));
					}
					if code == JOB_OBJECT_MSG_NEW_PROCESS {
						self.observe_process(job, overlapped as usize)?;
					}
				}
				Err(error) if error.code() == HRESULT::from_win32(WAIT_TIMEOUT.0) => {
					drained = true;
					break;
				}
				Err(error) => return Err(Error::other(error)),
			}
		}
		if !drained {
			return Ok(ControlFlow::Continue(()));
		}
		let mut index = 0;
		while index < self.pending.len() {
			match unsafe { WaitForSingleObject(HANDLE(self.pending[index].as_raw_handle()), 0) } {
				WAIT_OBJECT_0 => {
					self.pending.swap_remove(index);
				}
				WAIT_TIMEOUT => {
					index += 1;
				}
				_ => return Err(Error::last_os_error()),
			}
		}
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
		if accounting.ActiveProcesses != 0 {
			return Ok(ControlFlow::Continue(()));
		}
		// Job accounting can reach zero before termination finishes, and notifications can be lost.
		if self.observed != accounting.TotalProcesses {
			return Err(Error::other(
				"cannot confirm job completion: process notification census is incomplete",
			));
		}
		Ok(if self.pending.is_empty() {
			ControlFlow::Break(())
		} else {
			ControlFlow::Continue(())
		})
	}
}

/// Wait for a job and its observed processes to complete.
#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(job_port)))]
pub(crate) fn wait_on_job(
	job_port: &JobPort,
	timeout: Option<Duration>,
) -> Result<ControlFlow<()>> {
	let started = Instant::now();
	loop {
		match job_port.completion.try_lock() {
			Ok(mut completion) => {
				if completion
					.poll(job_port.job, job_port.completion_port)?
					.is_break()
				{
					return Ok(ControlFlow::Break(()));
				}
			}
			Err(TryLockError::WouldBlock) => {}
			Err(TryLockError::Poisoned(_)) => {
				return Err(Error::other("job completion state is poisoned"));
			}
		}
		let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
		if remaining == Some(Duration::ZERO) {
			return Ok(ControlFlow::Continue(()));
		}
		std::thread::sleep(
			remaining
				.unwrap_or(Duration::from_millis(10))
				.min(Duration::from_millis(10)),
		);
	}
}

#[cfg(test)]
mod wait_tests {
	use super::*;

	#[test]
	fn incomplete_census_cannot_confirm_an_empty_job() -> Result<()> {
		let job = OwnedHandle(unsafe { CreateJobObjectW(None, None) }?);
		let port =
			OwnedHandle(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }?);
		let mut completion = JobCompletion {
			observed: 1,
			..Default::default()
		};
		for _ in 0..2 {
			let error = completion
				.poll(JobHandle(job.0), PortHandle(port.0))
				.unwrap_err();
			assert!(
				error
					.to_string()
					.contains("process notification census is incomplete")
			);
		}
		Ok(())
	}

	#[test]
	fn process_census_overflow_does_not_advance_observation() {
		let mut completion = JobCompletion {
			observed: u32::MAX,
			..Default::default()
		};
		assert!(
			completion
				.observe_process(JobHandle(INVALID_HANDLE_VALUE), 0)
				.is_err()
		);
		assert_eq!(completion.observed, u32::MAX);
	}

	#[test]
	fn invalid_job_is_an_error_even_for_nonblocking_wait() {
		let job_port = JobPort {
			job: JobHandle(INVALID_HANDLE_VALUE),
			completion_port: PortHandle(INVALID_HANDLE_VALUE),
			completion: Arc::default(),
		};
		assert!(wait_on_job(&job_port, Some(Duration::ZERO)).is_err());
	}
}
