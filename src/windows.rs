//! Windows API support functions.

use std::{
	io::{Error, Result},
	ops::ControlFlow,
	os::windows::io::{
		AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle as OwnedJobHandle,
	},
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
			SystemServices::{
				JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS, JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT,
				JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO, JOB_OBJECT_MSG_END_OF_JOB_TIME,
				JOB_OBJECT_MSG_END_OF_PROCESS_TIME, JOB_OBJECT_MSG_EXIT_PROCESS,
				JOB_OBJECT_MSG_JOB_CYCLE_TIME_LIMIT, JOB_OBJECT_MSG_JOB_MEMORY_LIMIT,
				JOB_OBJECT_MSG_NEW_PROCESS, JOB_OBJECT_MSG_NOTIFICATION_LIMIT,
				JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT, JOB_OBJECT_MSG_SILO_TERMINATED,
			},
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

/// A handle to the job object of a `JobObjectChild`, shared with it.
///
/// Get one with `JobObjectChild::job_handle()`. Unlike the child, it can be cloned and sent to other
/// threads, so the job can be terminated without the child: for example while another thread waits
/// on it.
///
/// The job is closed once the child and every handle to it have been dropped. With `KillOnDrop`,
/// that's when the processes still in it are killed, so holding a handle keeps them
/// running after the child is dropped. A child unwrapped with `into_inner()` leaves the job open.
///
/// [`AsHandle`] borrows the job's handle, to use it with other job object APIs.
#[derive(Clone, Debug)]
pub struct JobObjectHandle(Arc<OwnedJobHandle>);

impl JobObjectHandle {
	/// Terminate every process in the job, without waiting for them to exit.
	///
	/// This is what `start_kill()` does on the child, but it doesn't need the child. Each process
	/// exits with `exit_code`. See [TerminateJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-terminatejobobject).
	#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
	pub fn terminate(&self, exit_code: u32) -> Result<()> {
		unsafe { TerminateJobObject(HANDLE(self.0.as_raw_handle()), exit_code) }
			.map_err(Error::other)
	}
}

impl AsHandle for JobObjectHandle {
	fn as_handle(&self) -> BorrowedHandle<'_> {
		self.0.as_handle()
	}
}

#[derive(Clone, Copy, Debug)]
pub struct PortHandle(pub HANDLE);

unsafe impl Send for PortHandle {}
unsafe impl Sync for PortHandle {}

/// A JobObject and its associated completion port.
///
/// This struct closes the completion port when dropped; the job is closed when the last
/// `JobObjectHandle` to it is.
#[derive(Debug)]
pub(crate) struct JobPort {
	pub job: JobObjectHandle,
	pub completion_port: PortHandle,
	completion: Arc<Mutex<JobCompletion>>,
}

impl JobPort {
	pub(crate) fn detach(self) {
		// Preserve into_inner's existing job lifetime, but release the port and completion state.
		std::mem::forget(self.job.clone());
	}
}

#[cfg(feature = "tokio1")]
impl JobPort {
	/// A bounded blocking poll must own every handle it uses if its async caller is cancelled.
	pub(crate) fn try_clone(&self) -> Result<Self> {
		use std::os::windows::io::IntoRawHandle;
		// SAFETY: self owns the port for this borrow; duplication creates an independent owner.
		let port =
			unsafe { BorrowedHandle::borrow_raw(self.completion_port.0.0) }.try_clone_to_owned()?;
		Ok(Self {
			job: self.job.clone(),
			completion_port: PortHandle(HANDLE(port.into_raw_handle())),
			completion: Arc::clone(&self.completion),
		})
	}
}

impl Drop for JobPort {
	fn drop(&mut self) {
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
		completion: Arc::new(Mutex::new(JobCompletion::default())),
		job: JobObjectHandle(Arc::new(unsafe {
			OwnedJobHandle::from_raw_handle(job.into_raw().0)
		})),
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

/// Completion notifications are hints, not process-exit fences. Retain handles from NEW_PROCESS
/// packets and reconcile the notification census with job accounting before reporting completion.
/// This assumes our private port has one logical consumer and nobody externally adds processes,
/// changes the port association, or enables breakaway via a borrowed job handle.
#[derive(Debug, Default)]
struct JobCompletion {
	observed: u32,
	pending: Vec<OwnedJobHandle>,
}

impl JobCompletion {
	fn observe_process(&mut self, job: HANDLE, pid: usize) -> Result<()> {
		let observed = self
			.observed
			.checked_add(1)
			.ok_or_else(|| Error::other("job process census overflow"))?;
		let pid = u32::try_from(pid).map_err(Error::other)?;
		if pid == 0 {
			return Err(Error::other("invalid job process notification"));
		}
		// SAFETY: no borrowed memory crosses the call; a successful open returns a new owner.
		let handle = match unsafe {
			OpenProcess(
				PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
				false,
				pid,
			)
		} {
			Ok(handle) => {
				// SAFETY: transfer the newly opened handle exactly once.
				unsafe { OwnedJobHandle::from_raw_handle(handle.0) }
			}
			Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
				// The process object no longer exists; it cannot still be executing.
				self.observed = observed;
				return Ok(());
			}
			Err(error) => return Err(Error::other(error)),
		};
		let mut belongs = Default::default();
		// SAFETY: both handles are owned throughout the call; belongs is writable BOOL storage.
		unsafe { IsProcessInJob(HANDLE(handle.as_raw_handle()), Some(job), &mut belongs) }?;
		if belongs.as_bool() {
			self.pending.push(handle);
		}
		// If the PID was recycled outside this job, the originally notified process has exited.
		self.observed = observed;
		Ok(())
	}

	fn drain_port(&mut self, job_port: &JobPort, job: HANDLE, budget: &mut usize) -> Result<bool> {
		while *budget > 0 {
			*budget -= 1;
			let mut code = 0;
			let mut key = 0;
			let mut overlapped = std::ptr::null_mut::<OVERLAPPED>();
			// SAFETY: JobPort owns the port; outputs are valid local storage. The returned
			// overlapped value is a numeric PID for job packets and is never dereferenced.
			match unsafe {
				GetQueuedCompletionStatus(
					job_port.completion_port.0,
					&mut code,
					&mut key,
					&mut overlapped,
					0,
				)
			} {
				Ok(()) => {
					if key != job.0 as usize {
						return Err(Error::other("unexpected job completion key"));
					}
					match code {
						JOB_OBJECT_MSG_NEW_PROCESS => {
							self.observe_process(job, overlapped as usize)?
						}
						JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO
						| JOB_OBJECT_MSG_EXIT_PROCESS
						| JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS
						| JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT
						| JOB_OBJECT_MSG_END_OF_JOB_TIME
						| JOB_OBJECT_MSG_END_OF_PROCESS_TIME
						| JOB_OBJECT_MSG_JOB_CYCLE_TIME_LIMIT
						| JOB_OBJECT_MSG_JOB_MEMORY_LIMIT
						| JOB_OBJECT_MSG_NOTIFICATION_LIMIT
						| JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT
						| JOB_OBJECT_MSG_SILO_TERMINATED => {}
						// No individual notification is a completion fence. Unknown packets are errors.
						_ => return Err(Error::other("unexpected job completion message")),
					}
				}
				Err(error) if error.code() == HRESULT::from_win32(WAIT_TIMEOUT.0) => {
					return Ok(true);
				}
				Err(error) => return Err(Error::other(error)),
			}
		}
		Ok(false)
	}

	fn reap_pending(&mut self) -> Result<()> {
		let mut index = 0;
		while index < self.pending.len() {
			// SAFETY: pending owns each process handle until its signalled state is observed.
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
		Ok(())
	}

	fn poll(
		&mut self,
		job_port: &JobPort,
		#[cfg(test)] after_initial_drain: impl FnOnce(),
	) -> Result<ControlFlow<()>> {
		let job = HANDLE(job_port.job.as_handle().as_raw_handle());
		// Both drains share this budget, including empty probes, to bound nonblocking polls.
		let mut budget = 64;
		if !self.drain_port(job_port, job, &mut budget)? {
			return Ok(ControlFlow::Continue(()));
		}
		// Tests schedule native packet delivery in the real drain/accounting window.
		#[cfg(test)]
		after_initial_drain();
		self.reap_pending()?;
		let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
		// SAFETY: JobPort owns the job; the output pointer and length describe this exact struct.
		unsafe {
			QueryInformationJobObject(
				Some(job),
				JobObjectBasicAccountingInformation,
				&mut accounting as *mut _ as _,
				std::mem::size_of_val(&accounting)
					.try_into()
					.map_err(Error::other)?,
				None,
			)
		}?;
		if accounting.ActiveProcesses != 0 {
			return Ok(ControlFlow::Continue(()));
		}
		if self.observed < accounting.TotalProcesses {
			// A notification can arrive after the first empty probe but before this accounting
			// snapshot. Recheck the queue before rejecting its census, within the same budget.
			// Zero accounting is not a fence for notification posting or process-handle signalling.
			if !self.drain_port(job_port, job, &mut budget)? {
				return Ok(ControlFlow::Continue(()));
			}
			self.reap_pending()?;
		}
		// Delivery is not guaranteed. An incomplete census after the recheck is an error, not
		// a completion claim; every retained handle must also have been observed signalled.
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

/// Wait for the job and every retained process object, not merely the next port packet.
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
					.poll(
						job_port,
						#[cfg(test)]
						|| {},
					)?
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
mod completion_tests {
	use super::*;
	use std::{
		os::windows::process::CommandExt,
		process::{Child, Command, Stdio},
	};
	use windows::Win32::System::{IO::PostQueuedCompletionStatus, Threading::CREATE_NO_WINDOW};

	struct Fixture {
		child: Child,
		port: JobPort,
	}

	impl Fixture {
		fn new() -> Self {
			let mut child = Command::new("cmd.exe")
				.args(["/D", "/C", "exit 0"])
				.creation_flags((CREATE_NO_WINDOW | CREATE_SUSPENDED).0)
				.stdin(Stdio::null())
				.stdout(Stdio::null())
				.stderr(Stdio::null())
				.spawn()
				.unwrap();
			let port = match make_job_object(HANDLE(child.as_raw_handle()), true) {
				Ok(port) => port,
				Err(error) => {
					child.kill().unwrap();
					child.wait().unwrap();
					panic!("{error}");
				}
			};
			Self { child, port }
		}

		fn post(&self, code: u32, key: usize) {
			// SAFETY: the fixture owns this live port. Null overlapped is a deliberately invalid
			// process ID for NEW_PROCESS, or an unused value for other notification types.
			unsafe { PostQueuedCompletionStatus(self.port.completion_port.0, code, key, None) }
				.unwrap();
		}

		fn take_packet(&self) -> (u32, usize, *mut OVERLAPPED) {
			let mut code = 0;
			let mut key = 0;
			let mut overlapped = std::ptr::null_mut();
			// SAFETY: this fixture owns the port and all output storage. The job's numeric
			// process-ID payload is retained as an opaque value, never dereferenced.
			unsafe {
				GetQueuedCompletionStatus(
					self.port.completion_port.0,
					&mut code,
					&mut key,
					&mut overlapped,
					5_000,
				)
			}
			.unwrap();
			(code, key, overlapped)
		}

		fn repost(&self, packet: (u32, usize, *mut OVERLAPPED)) {
			// SAFETY: the fixture owns the port; replay exactly the native notification's opaque
			// payload. PostQueuedCompletionStatus stores the value without dereferencing it.
			unsafe {
				PostQueuedCompletionStatus(
					self.port.completion_port.0,
					packet.0,
					packet.1,
					Some(packet.2.cast_const()),
				)
			}
			.unwrap();
		}

		fn terminate_and_wait(&self) {
			self.port.job.terminate(1).unwrap();
			// SAFETY: Child retains this exact process object throughout the bounded native wait.
			assert_eq!(
				unsafe { WaitForSingleObject(HANDLE(self.child.as_raw_handle()), 5_000) },
				WAIT_OBJECT_0
			);
		}
	}

	impl Drop for Fixture {
		fn drop(&mut self) {
			self.port.job.terminate(1).unwrap();
			// SAFETY: Child owns the exact process handle and remains alive during the bounded wait.
			assert_eq!(
				unsafe { WaitForSingleObject(HANDLE(self.child.as_raw_handle()), 5_000) },
				WAIT_OBJECT_0
			);
			self.child.wait().unwrap();
		}
	}

	#[test]
	fn new_process_notification_does_not_complete_a_live_job() {
		let fixture = Fixture::new();
		assert!(
			wait_on_job(&fixture.port, Some(Duration::ZERO))
				.unwrap()
				.is_continue()
		);
		fixture.port.job.terminate(1).unwrap();
		assert!(
			wait_on_job(&fixture.port, Some(Duration::from_secs(5)))
				.unwrap()
				.is_break()
		);
	}

	#[test]
	fn packet_arriving_between_drain_and_zero_accounting_is_not_lost() {
		let fixture = Fixture::new();
		let packet = fixture.take_packet();
		assert_eq!(packet.0, JOB_OBJECT_MSG_NEW_PROCESS);
		assert_eq!(packet.2 as usize, fixture.child.id() as usize);
		let mut completion = fixture.port.completion.lock().unwrap();
		let called = std::cell::Cell::new(false);
		let result = completion.poll(&fixture.port, || {
			called.set(true);
			// The initial drain has already observed WAIT_TIMEOUT. Deliver the held native
			// NEW_PROCESS now, then make real accounting reach zero before poll queries it.
			fixture.repost(packet);
			fixture.terminate_and_wait();
		});
		assert!(called.get(), "native delivery seam was not exercised");
		assert!(
			result.is_ok(),
			"queued NEW_PROCESS was mistaken for loss: {result:?}"
		);
		assert!(result.unwrap().is_break());
		assert_eq!(completion.observed, 1);
		assert!(completion.pending.is_empty());
	}

	#[test]
	fn both_drains_share_one_budget_including_the_initial_empty_probe() {
		let fixture = Fixture::new();
		let packet = fixture.take_packet();
		assert_eq!(packet.0, JOB_OBJECT_MSG_NEW_PROCESS);
		// 63 native IOCP packets plus the first empty probe exhaust the 64-dequeue budget.
		for _ in 0..63 {
			fixture.post(JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT, packet.1);
		}
		let mut completion = fixture.port.completion.lock().unwrap();
		let result = completion.poll(&fixture.port, || {
			fixture.repost(packet);
			fixture.terminate_and_wait();
		});
		assert!(
			matches!(result, Ok(ControlFlow::Continue(()))),
			"budget exhausted before post-zero drain: {result:?}"
		);
		assert_eq!(
			completion.observed, 0,
			"a fresh second budget consumed the late packet"
		);
		let queued = fixture.take_packet();
		assert_eq!(
			queued, packet,
			"late NEW_PROCESS must remain queued for the next poll"
		);
		fixture.repost(queued);
		assert!(completion.poll(&fixture.port, || {}).unwrap().is_break());
		assert_eq!(completion.observed, 1);
	}

	#[test]
	fn more_than_one_budget_defers_the_packet_and_retains_the_live_handle() {
		let fixture = Fixture::new();
		let packet = fixture.take_packet();
		assert_eq!(packet.0, JOB_OBJECT_MSG_NEW_PROCESS);
		for _ in 0..65 {
			fixture.post(JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT, packet.1);
		}
		fixture.repost(packet);
		let mut completion = fixture.port.completion.lock().unwrap();
		let called = std::cell::Cell::new(false);
		assert!(
			completion
				.poll(&fixture.port, || {
					called.set(true);
				})
				.unwrap()
				.is_continue()
		);
		assert!(
			!called.get(),
			"a full first drain must yield before accounting"
		);
		assert_eq!(completion.observed, 0);
		assert!(completion.poll(&fixture.port, || {}).unwrap().is_continue());
		assert_eq!(completion.observed, 1);
		assert_eq!(completion.pending.len(), 1);
		// SAFETY: completion owns this newly observed process handle. It is still suspended/live.
		assert_eq!(
			unsafe { WaitForSingleObject(HANDLE(completion.pending[0].as_raw_handle()), 0) },
			WAIT_TIMEOUT
		);
		fixture.terminate_and_wait();
		assert!(completion.poll(&fixture.port, || {}).unwrap().is_break());
		assert!(completion.pending.is_empty());
	}

	#[test]
	fn unexpected_completion_key_is_an_error() {
		let fixture = Fixture::new();
		fixture.post(JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO, 0);
		assert!(
			wait_on_job(&fixture.port, Some(Duration::ZERO))
				.unwrap_err()
				.to_string()
				.contains("completion key")
		);
	}

	#[test]
	fn unknown_completion_message_is_an_error() {
		let fixture = Fixture::new();
		fixture.post(
			u32::MAX,
			fixture.port.job.as_handle().as_raw_handle() as usize,
		);
		assert!(
			wait_on_job(&fixture.port, Some(Duration::ZERO))
				.unwrap_err()
				.to_string()
				.contains("completion message")
		);
	}

	#[test]
	fn invalid_process_notification_is_an_error() {
		let fixture = Fixture::new();
		fixture.post(
			JOB_OBJECT_MSG_NEW_PROCESS,
			fixture.port.job.as_handle().as_raw_handle() as usize,
		);
		assert!(
			wait_on_job(&fixture.port, Some(Duration::ZERO))
				.unwrap_err()
				.to_string()
				.contains("invalid job process")
		);
	}

	#[test]
	fn missing_process_notification_is_not_successful_completion() {
		let fixture = Fixture::new();
		let mut code = 0;
		let mut key = 0;
		let mut overlapped = std::ptr::null_mut::<OVERLAPPED>();
		// SAFETY: all outputs are valid and the fixture owns the port. Deliberately consume
		// the real NEW_PROCESS packet to model an incomplete notification census.
		unsafe {
			GetQueuedCompletionStatus(
				fixture.port.completion_port.0,
				&mut code,
				&mut key,
				&mut overlapped,
				5_000,
			)
		}
		.unwrap();
		assert_eq!(code, JOB_OBJECT_MSG_NEW_PROCESS);
		fixture.port.job.terminate(1).unwrap();
		// SAFETY: the child owns this live process handle throughout the bounded wait.
		assert_eq!(
			unsafe { WaitForSingleObject(HANDLE(fixture.child.as_raw_handle()), 5_000) },
			WAIT_OBJECT_0
		);
		assert!(
			wait_on_job(&fixture.port, Some(Duration::ZERO))
				.unwrap_err()
				.to_string()
				.contains("census is incomplete")
		);
	}

	#[test]
	fn invalid_completion_port_is_not_a_timeout() {
		let fixture = Fixture::new();
		// The invalid port is never used as an owned handle and the real port stays open.
		let invalid = JobPort {
			job: fixture.port.job.clone(),
			completion_port: PortHandle(INVALID_HANDLE_VALUE),
			completion: Arc::clone(&fixture.port.completion),
		};
		assert!(wait_on_job(&invalid, Some(Duration::ZERO)).is_err());
	}
}
