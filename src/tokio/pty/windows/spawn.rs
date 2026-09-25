//! Exact `CreateProcessW` spawning for a prepared ConPTY command.

use std::{
	ffi::c_void,
	io,
	mem::size_of,
	os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
	sync::{Arc, Mutex, OnceLock},
};

use windows::{
	Win32::{
		Foundation::{
			DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
			WAIT_TIMEOUT, WIN32_ERROR,
		},
		System::Threading::{
			CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
			GetCurrentProcess, GetProcessIdOfThread, INFINITE, OpenProcess, PROCESS_CREATION_FLAGS,
			PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, STARTF_USESTDHANDLES,
			STARTUPINFOEXW, TerminateProcess, WaitForSingleObject,
		},
	},
	core::{PCWSTR, PWSTR},
};

use super::{
	PreparedWindowsCommand, attributes::AttributeList, child::ConPtyChild,
	environment::PreparedEnvironment,
};

pub(super) fn spawn(
	mut command: PreparedWindowsCommand,
	attributes: &AttributeList,
	input_server: OwnedHandle,
	output_server: OwnedHandle,
) -> io::Result<SpawnedChild> {
	let startup = startup_info(attributes);

	let mut flags = PROCESS_CREATION_FLAGS(command.creation.spawn_creation_flags())
		| EXTENDED_STARTUPINFO_PRESENT;
	let environment = match &command.environment {
		PreparedEnvironment::Inherit => None,
		PreparedEnvironment::Block(block) => {
			flags |= CREATE_UNICODE_ENVIRONMENT;
			Some(block.as_ptr().cast::<c_void>())
		}
	};
	let current_dir = command
		.current_dir
		.as_ref()
		.map_or(PCWSTR::null(), |directory| PCWSTR(directory.as_ptr()));
	let mut information = PROCESS_INFORMATION::default();

	// SAFETY: every pointer references live, NUL-terminated or explicitly bounded storage for the
	// duration of the call. The command line is uniquely mutable, the startup attribute list owns its
	// aligned backing storage, no native handles are inherited, and STARTF_USESTDHANDLES with three null
	// slots asks ConPTY to install its console handles. The process-information value is writable output
	// storage.
	let created = unsafe {
		CreateProcessW(
			PCWSTR(command.application_name.as_ptr()),
			Some(PWSTR(command.command_line.as_mut_ptr())),
			None,
			None,
			false,
			flags,
			environment,
			current_dir,
			&startup.StartupInfo,
			&mut information,
		)
	};
	// ConPTY retains these synchronous endpoints internally. Dropping the parent copies before any
	// validation or handle duplication lets both host-facing channels observe closure on every path.
	drop(input_server);
	drop(output_server);
	created.map_err(win32_io_error)?;

	let handles = SpawnedProcess::new(information)?;
	let cleanup = handles.cleanup()?;
	let child = handles.into_child(command.creation.kills_on_drop());
	Ok(SpawnedChild { child, cleanup })
}

fn startup_info(attributes: &AttributeList) -> STARTUPINFOEXW {
	let mut startup = STARTUPINFOEXW::default();
	startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
	startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
	startup.StartupInfo.hStdInput = HANDLE::default();
	startup.StartupInfo.hStdOutput = HANDLE::default();
	startup.StartupInfo.hStdError = HANDLE::default();
	startup.lpAttributeList = attributes.as_ptr();
	startup
}

#[derive(Debug)]
pub(super) struct SpawnedChild {
	pub(super) child: ConPtyChild,
	pub(super) cleanup: SpawnCleanup,
}

#[derive(Debug)]
pub(super) struct SpawnCleanup {
	process: Option<OwnedHandle>,
}

impl SpawnCleanup {
	pub(super) fn disarm(&mut self) {
		self.process.take();
	}

	pub(super) fn rollback(&mut self) -> io::Result<()> {
		self.process.take().map_or(Ok(()), terminate_and_reap)
	}
}

impl Drop for SpawnCleanup {
	fn drop(&mut self) {
		let _ = self.rollback();
	}
}

struct SpawnedProcess {
	process: Option<OwnedHandle>,
	primary_thread: Option<OwnedHandle>,
	pid: u32,
}

impl SpawnedProcess {
	fn new(information: PROCESS_INFORMATION) -> io::Result<Self> {
		if information.hProcess.is_invalid() || information.hThread.is_invalid() {
			cleanup_invalid_information(information);
			return Err(io::Error::other(
				"CreateProcessW returned invalid process information",
			));
		}

		// SAFETY: CreateProcessW returned two distinct owned handles, each transferred exactly once.
		let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) };
		// SAFETY: as above, ownership of the primary-thread handle is transferred exactly once.
		let primary_thread = unsafe { OwnedHandle::from_raw_handle(information.hThread.0) };
		Ok(Self {
			process: Some(process),
			primary_thread: Some(primary_thread),
			pid: information.dwProcessId,
		})
	}

	fn cleanup(&self) -> io::Result<SpawnCleanup> {
		let process = self
			.process
			.as_ref()
			.expect("a spawned process guard must own its process handle");
		Ok(SpawnCleanup {
			process: Some(duplicate(process)?),
		})
	}

	fn into_child(mut self, kill_on_drop: bool) -> ConPtyChild {
		let process = self
			.process
			.take()
			.expect("a spawned process guard must own its process handle");
		let primary_thread = self
			.primary_thread
			.take()
			.expect("a spawned process guard must own its primary-thread handle");
		ConPtyChild::new(process, primary_thread, self.pid, kill_on_drop)
	}
}

impl Drop for SpawnedProcess {
	fn drop(&mut self) {
		if let Some(process) = self.process.take() {
			let _ = terminate_and_reap(process);
		}
	}
}

fn cleanup_invalid_information(information: PROCESS_INFORMATION) {
	let thread = (!information.hThread.is_invalid()).then(|| {
		// SAFETY: a non-invalid CreateProcessW output handle is uniquely owned here.
		unsafe { OwnedHandle::from_raw_handle(information.hThread.0) }
	});
	let pid = thread.as_ref().map_or(0, |thread| {
		// SAFETY: thread owns the live primary-thread handle returned by CreateProcessW.
		unsafe { GetProcessIdOfThread(HANDLE(thread.as_raw_handle())) }
	});
	let pid = if pid != 0 && (information.dwProcessId == 0 || information.dwProcessId == pid) {
		pid
	} else {
		0
	};
	let process = if !information.hProcess.is_invalid() {
		// SAFETY: a non-invalid CreateProcessW output handle is uniquely owned here.
		Some(unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) })
	} else if pid != 0 {
		// SAFETY: pid identifies the process CreateProcessW reported creating. The recovered handle is
		// non-inheritable and is transferred exactly once if it can still be opened.
		unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }
			.ok()
			.map(|handle| unsafe { OwnedHandle::from_raw_handle(handle.0) })
	} else {
		None
	};
	if let Some(process) = process {
		let _ = terminate_and_reap(process);
	}
}

fn duplicate(handle: &OwnedHandle) -> io::Result<OwnedHandle> {
	// SAFETY: the current-process pseudo-handle and source process handle are live, duplicate points
	// to writable storage, and ownership of the non-inheritable result is transferred exactly once.
	unsafe {
		let current = GetCurrentProcess();
		let mut duplicate = HANDLE::default();
		DuplicateHandle(
			current,
			HANDLE(handle.as_raw_handle()),
			current,
			&mut duplicate,
			0,
			false,
			DUPLICATE_SAME_ACCESS,
		)
		.map_err(win32_io_error)?;
		Ok(OwnedHandle::from_raw_handle(duplicate.0))
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitOutcome {
	Signaled,
	Timeout,
}

/// Sole owner of the exact handle selected for rollback cleanup.
///
/// `process` is `Some` from construction until `Drop`; moving this value transfers that ownership
/// unchanged between the caller, reaper handoff cell, reaper thread, and quarantine. Only `Drop`
/// takes and closes the handle.
#[derive(Debug)]
struct ReapHandle {
	process: Option<OwnedHandle>,
	#[cfg(test)]
	released: Option<std::sync::mpsc::Sender<usize>>,
}

impl ReapHandle {
	fn new(process: OwnedHandle) -> Self {
		Self {
			process: Some(process),
			#[cfg(test)]
			released: None,
		}
	}

	#[cfg(test)]
	fn tracked(process: OwnedHandle, released: std::sync::mpsc::Sender<usize>) -> Self {
		Self {
			process: Some(process),
			released: Some(released),
		}
	}

	fn raw(&self) -> HANDLE {
		HANDLE(
			self.process
				.as_ref()
				.expect("a reap handle owns its exact process handle until release")
				.as_raw_handle(),
		)
	}
}

impl Drop for ReapHandle {
	fn drop(&mut self) {
		#[cfg(test)]
		let raw = self.raw().0 as usize;
		// Dropping the OwnedHandle closes exactly the handle transferred into this owner. Do this before
		// notifying tests so release observations cannot race the close.
		drop(self.process.take());
		#[cfg(test)]
		if let Some(released) = self.released.take() {
			let _ = released.send(raw);
		}
	}
}

trait CleanupOperations: Clone + Send + Sync + 'static {
	fn terminate(&self, process: HANDLE) -> io::Result<()>;
	fn wait(&self, process: HANDLE, timeout: u32) -> io::Result<WaitOutcome>;

	/// Start the named reaper task. Returning an error means the task was not started.
	fn spawn_reaper(&self, task: Box<dyn FnOnce() + Send>) -> io::Result<()>;

	fn quarantine(&self, process: ReapHandle);
	fn take_quarantined(&self) -> Vec<ReapHandle>;
}

#[derive(Clone, Copy, Debug)]
struct WindowsCleanupOperations;

impl CleanupOperations for WindowsCleanupOperations {
	fn terminate(&self, process: HANDLE) -> io::Result<()> {
		// SAFETY: every production ReapHandle originates from CreateProcessW, DuplicateHandle with
		// DUPLICATE_SAME_ACCESS, or OpenProcess with PROCESS_TERMINATE. Its OwnedHandle remains inside the
		// ReapHandle for this call, so the exact handle stays open throughout the operation.
		unsafe { TerminateProcess(process, 1) }.map_err(win32_io_error)
	}

	fn wait(&self, process: HANDLE, timeout: u32) -> io::Result<WaitOutcome> {
		// SAFETY: every production ReapHandle originates from CreateProcessW, DuplicateHandle with
		// DUPLICATE_SAME_ACCESS, or OpenProcess with PROCESS_SYNCHRONIZE. Its OwnedHandle remains inside the
		// ReapHandle for this call. Callers pass either zero for a nonblocking probe or INFINITE on the
		// dedicated reaper thread.
		match unsafe { WaitForSingleObject(process, timeout) } {
			WAIT_OBJECT_0 => Ok(WaitOutcome::Signaled),
			WAIT_TIMEOUT => Ok(WaitOutcome::Timeout),
			WAIT_FAILED => Err(io::Error::last_os_error()),
			other => Err(io::Error::other(format!(
				"unexpected ConPTY rollback wait result {}",
				other.0
			))),
		}
	}

	fn spawn_reaper(&self, task: Box<dyn FnOnce() + Send>) -> io::Result<()> {
		std::thread::Builder::new()
			.name("process-wrap-conpty-reaper".into())
			.spawn(task)
			.map(drop)
	}

	fn quarantine(&self, process: ReapHandle) {
		quarantined_processes()
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.push(process);
	}

	fn take_quarantined(&self) -> Vec<ReapHandle> {
		std::mem::take(
			&mut *quarantined_processes()
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner),
		)
	}
}

fn quarantined_processes() -> &'static Mutex<Vec<ReapHandle>> {
	static QUARANTINED: OnceLock<Mutex<Vec<ReapHandle>>> = OnceLock::new();
	QUARANTINED.get_or_init(|| Mutex::new(Vec::new()))
}

#[derive(Debug)]
struct ReaperStartError {
	error: io::Error,
	process: ReapHandle,
}

fn terminate_and_reap(process: OwnedHandle) -> io::Result<()> {
	terminate_and_reap_with(ReapHandle::new(process), WindowsCleanupOperations)
}

fn terminate_and_reap_with<O: CleanupOperations>(
	process: ReapHandle,
	operations: O,
) -> io::Result<()> {
	let retry_error = retry_quarantined(operations.clone());
	let termination_error = operations.terminate(process.raw()).err();

	let current_error = match operations.wait(process.raw(), 0) {
		Ok(WaitOutcome::Signaled) => None,
		Ok(WaitOutcome::Timeout) => {
			if let Some(error) = termination_error.as_ref() {
				warn_cleanup(
					error,
					"failed to terminate an unsignaled ConPTY rollback process",
				);
			}
			let scheduling_error = defer_reap(process, operations.clone()).err();
			termination_error.or(scheduling_error)
		}
		Err(wait_error) => {
			if let Some(error) = termination_error.as_ref() {
				warn_cleanup(error, "failed to terminate a ConPTY rollback process");
			}
			warn_cleanup(&wait_error, "failed to probe a ConPTY rollback process");
			let _ = defer_reap(process, operations.clone());
			Some(wait_error)
		}
	};

	current_error.or(retry_error).map_or(Ok(()), Err)
}

fn retry_quarantined<O: CleanupOperations>(operations: O) -> Option<io::Error> {
	let mut first_error = None;
	for process in operations.take_quarantined() {
		if let Err(failure) = start_reaper(process, operations.clone()) {
			warn_cleanup(
				&failure.error,
				"failed to retry a quarantined ConPTY rollback process",
			);
			if first_error.is_none() {
				first_error = Some(failure.error);
			}
			operations.quarantine(failure.process);
		}
	}
	first_error
}

fn defer_reap<O: CleanupOperations>(process: ReapHandle, operations: O) -> io::Result<()> {
	match start_reaper(process, operations.clone()) {
		Ok(()) => Ok(()),
		Err(failure) => {
			warn_cleanup(
				&failure.error,
				"failed to start a ConPTY rollback reaper; quarantining the exact process handle",
			);
			operations.quarantine(failure.process);
			Err(failure.error)
		}
	}
}

fn start_reaper<O: CleanupOperations>(
	process: ReapHandle,
	operations: O,
) -> Result<(), ReaperStartError> {
	// The caller's Arc keeps the transfer cell and exact handle alive if thread creation fails. On
	// success, the task takes the handle before waiting and the detached JoinHandle owns no process
	// resource.
	let transfer = Arc::new(Mutex::new(Some(process)));
	let task_transfer = Arc::clone(&transfer);
	let task_operations = operations.clone();
	let task = Box::new(move || {
		let process = task_transfer
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
			.expect("a started ConPTY reaper receives the exact handle once");
		match task_operations.wait(process.raw(), INFINITE) {
			Ok(WaitOutcome::Signaled) => drop(process),
			Ok(WaitOutcome::Timeout) => {
				let error = io::Error::other(
					"an infinite ConPTY rollback reaper wait unexpectedly timed out",
				);
				warn_cleanup(
					&error,
					"ConPTY rollback reaper could not establish process completion; quarantining the exact process handle",
				);
				task_operations.quarantine(process);
			}
			Err(error) => {
				warn_cleanup(
					&error,
					"ConPTY rollback reaper wait failed; quarantining the exact process handle",
				);
				task_operations.quarantine(process);
			}
		}
	});

	if let Err(error) = operations.spawn_reaper(task) {
		let process = transfer
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
			.expect("a failed ConPTY reaper start leaves the exact handle with its caller");
		return Err(ReaperStartError { error, process });
	}
	Ok(())
}

fn warn_cleanup(error: &io::Error, action: &'static str) {
	#[cfg(feature = "tracing")]
	tracing::warn!(?error, action, "secondary ConPTY cleanup failure");
	#[cfg(not(feature = "tracing"))]
	let _ = (error, action);
}

fn win32_io_error(error: windows::core::Error) -> io::Error {
	WIN32_ERROR::from_error(&error)
		.and_then(|error| i32::try_from(error.0).ok())
		.map_or_else(|| io::Error::other(error), io::Error::from_raw_os_error)
}

#[cfg(test)]
mod tests {
	use std::{
		collections::VecDeque,
		os::windows::io::FromRawHandle,
		sync::{
			Arc, Condvar, Mutex,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		thread::ThreadId,
		time::Duration,
	};

	use windows::{
		Win32::System::{
			Console::HPCON,
			Threading::{CreateEventW, STARTF_USESTDHANDLES},
		},
		core::PCWSTR,
	};

	use super::*;

	#[derive(Clone, Copy, Debug)]
	enum WaitPlan {
		Signaled,
		Timeout,
		Error(i32),
	}

	impl WaitPlan {
		fn result(self) -> io::Result<WaitOutcome> {
			match self {
				Self::Signaled => Ok(WaitOutcome::Signaled),
				Self::Timeout => Ok(WaitOutcome::Timeout),
				Self::Error(code) => Err(io::Error::from_raw_os_error(code)),
			}
		}
	}

	#[derive(Debug)]
	struct WaitRecord {
		handle: usize,
		timeout: u32,
		thread: ThreadId,
	}

	#[derive(Debug, Default)]
	struct ReaperGate {
		signaled: bool,
		waiters: usize,
	}

	#[derive(Debug)]
	struct TestState {
		terminate_error: Option<i32>,
		probes: Mutex<VecDeque<WaitPlan>>,
		spawn_failures: AtomicUsize,
		spawn_calls: AtomicUsize,
		waits: Mutex<Vec<WaitRecord>>,
		gate: Mutex<ReaperGate>,
		gate_changed: Condvar,
		quarantine: Mutex<Vec<ReapHandle>>,
	}

	#[derive(Clone, Debug)]
	struct TestOperations {
		state: Arc<TestState>,
	}

	impl TestOperations {
		fn new(
			terminate_error: Option<i32>,
			probes: impl IntoIterator<Item = WaitPlan>,
			spawn_failures: usize,
		) -> Self {
			Self {
				state: Arc::new(TestState {
					terminate_error,
					probes: Mutex::new(probes.into_iter().collect()),
					spawn_failures: AtomicUsize::new(spawn_failures),
					spawn_calls: AtomicUsize::new(0),
					waits: Mutex::new(Vec::new()),
					gate: Mutex::new(ReaperGate::default()),
					gate_changed: Condvar::new(),
					quarantine: Mutex::new(Vec::new()),
				}),
			}
		}

		fn wait_for_reaper(&self) {
			let gate = self.state.gate.lock().unwrap();
			let (gate, timeout) = self
				.state
				.gate_changed
				.wait_timeout_while(gate, Duration::from_secs(2), |gate| gate.waiters == 0)
				.unwrap();
			assert!(!timeout.timed_out(), "the injected reaper did not start");
			assert!(gate.waiters > 0);
		}

		fn signal_reapers(&self) {
			let mut gate = self.state.gate.lock().unwrap();
			gate.signaled = true;
			self.state.gate_changed.notify_all();
		}

		fn quarantine_len(&self) -> usize {
			self.state.quarantine.lock().unwrap().len()
		}

		fn spawn_calls(&self) -> usize {
			self.state.spawn_calls.load(Ordering::SeqCst)
		}
	}

	impl CleanupOperations for TestOperations {
		fn terminate(&self, _process: HANDLE) -> io::Result<()> {
			match self.state.terminate_error {
				Some(code) => Err(io::Error::from_raw_os_error(code)),
				None => Ok(()),
			}
		}

		fn wait(&self, process: HANDLE, timeout: u32) -> io::Result<WaitOutcome> {
			self.state.waits.lock().unwrap().push(WaitRecord {
				handle: process.0 as usize,
				timeout,
				thread: std::thread::current().id(),
			});
			if timeout == 0 {
				return self
					.state
					.probes
					.lock()
					.unwrap()
					.pop_front()
					.expect("each cleanup has an injected zero-time probe")
					.result();
			}
			assert_eq!(timeout, INFINITE);

			let mut gate = self.state.gate.lock().unwrap();
			gate.waiters += 1;
			self.state.gate_changed.notify_all();
			while !gate.signaled {
				gate = self.state.gate_changed.wait(gate).unwrap();
			}
			Ok(WaitOutcome::Signaled)
		}

		fn spawn_reaper(&self, task: Box<dyn FnOnce() + Send>) -> io::Result<()> {
			self.state.spawn_calls.fetch_add(1, Ordering::SeqCst);
			let fail = self
				.state
				.spawn_failures
				.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
					remaining.checked_sub(1)
				})
				.is_ok();
			if fail {
				return Err(io::Error::other("injected ConPTY reaper start failure"));
			}
			std::thread::Builder::new()
				.name("test-process-wrap-conpty-reaper".into())
				.spawn(task)
				.map(drop)
		}

		fn quarantine(&self, process: ReapHandle) {
			self.state.quarantine.lock().unwrap().push(process);
		}

		fn take_quarantined(&self) -> Vec<ReapHandle> {
			std::mem::take(&mut *self.state.quarantine.lock().unwrap())
		}
	}

	fn tracked_handle() -> (ReapHandle, usize, mpsc::Receiver<usize>) {
		// SAFETY: null security attributes and name request a new, unnamed event. On success the returned
		// handle is uniquely owned by this test and is transferred exactly once to OwnedHandle.
		let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.unwrap();
		let raw = event.0 as usize;
		// SAFETY: CreateEventW returned a new owned handle, transferred exactly once here.
		let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
		let (released, observe_release) = mpsc::channel();
		(ReapHandle::tracked(event, released), raw, observe_release)
	}

	fn assert_not_released(released: &mpsc::Receiver<usize>) {
		assert_eq!(
			released.try_recv(),
			Err(mpsc::TryRecvError::Empty),
			"the exact handle was released before its reaper completed"
		);
	}

	fn assert_released(released: &mpsc::Receiver<usize>, raw: usize) {
		assert_eq!(released.recv_timeout(Duration::from_secs(2)).unwrap(), raw);
	}

	#[test]
	fn requests_conpty_replacement_for_all_standard_handles() {
		let attributes = AttributeList::new(HPCON(42)).unwrap();
		let startup = startup_info(&attributes);
		assert_eq!(startup.StartupInfo.dwFlags, STARTF_USESTDHANDLES);
		assert_eq!(startup.StartupInfo.hStdInput, HANDLE::default());
		assert_eq!(startup.StartupInfo.hStdOutput, HANDLE::default());
		assert_eq!(startup.StartupInfo.hStdError, HANDLE::default());
	}

	#[test]
	fn unsignaled_termination_hands_the_exact_handle_to_an_off_thread_reaper() {
		let operations = TestOperations::new(None, [WaitPlan::Timeout], 0);
		let (process, raw, released) = tracked_handle();
		let (completed, observe_completion) = mpsc::channel();
		let caller_operations = operations.clone();
		let caller = std::thread::spawn(move || {
			let caller = std::thread::current().id();
			let result = terminate_and_reap_with(process, caller_operations);
			completed.send((caller, result)).unwrap();
		});

		let completion = observe_completion.recv_timeout(Duration::from_secs(2));
		if completion.is_err() {
			operations.signal_reapers();
			caller.join().unwrap();
			panic!("cleanup waited for eventual signaling on its caller thread");
		}
		let (caller_id, result) = completion.unwrap();
		result.unwrap();
		caller.join().unwrap();
		operations.wait_for_reaper();
		assert_not_released(&released);

		let waits = operations.state.waits.lock().unwrap();
		assert_eq!(waits.len(), 2);
		assert_eq!(waits[0].handle, raw);
		assert_eq!(waits[0].timeout, 0);
		assert_eq!(waits[0].thread, caller_id);
		assert_eq!(waits[1].handle, raw);
		assert_eq!(waits[1].timeout, INFINITE);
		assert_ne!(waits[1].thread, caller_id);
		drop(waits);

		operations.signal_reapers();
		assert_released(&released, raw);
	}

	#[test]
	fn failed_termination_of_an_unsignaled_process_is_reported_and_reaped() {
		let operations = TestOperations::new(Some(5), [WaitPlan::Timeout], 0);
		let (process, raw, released) = tracked_handle();

		let error = terminate_and_reap_with(process, operations.clone()).unwrap_err();
		assert_eq!(error.raw_os_error(), Some(5));
		operations.wait_for_reaper();
		assert_not_released(&released);
		assert_eq!(operations.quarantine_len(), 0);

		operations.signal_reapers();
		assert_released(&released, raw);
	}

	#[test]
	fn failed_termination_of_an_already_signaled_process_is_a_natural_exit_race() {
		let operations = TestOperations::new(Some(5), [WaitPlan::Signaled], 0);
		let (process, raw, released) = tracked_handle();

		terminate_and_reap_with(process, operations.clone()).unwrap();
		assert_eq!(operations.spawn_calls(), 0);
		assert_eq!(operations.quarantine_len(), 0);
		assert_released(&released, raw);
	}

	#[test]
	fn failed_zero_time_wait_is_reported_while_the_exact_handle_stays_owned() {
		let operations = TestOperations::new(None, [WaitPlan::Error(6)], 0);
		let (process, raw, released) = tracked_handle();

		let error = terminate_and_reap_with(process, operations.clone()).unwrap_err();
		assert_eq!(error.raw_os_error(), Some(6));
		operations.wait_for_reaper();
		assert_not_released(&released);
		assert_eq!(operations.quarantine_len(), 0);

		operations.signal_reapers();
		assert_released(&released, raw);
	}

	#[test]
	fn failed_reaper_start_quarantines_then_retries_the_exact_handle() {
		let operations = TestOperations::new(None, [WaitPlan::Timeout, WaitPlan::Signaled], 1);
		let (process, raw, released) = tracked_handle();

		let error = terminate_and_reap_with(process, operations.clone()).unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::Other);
		assert_eq!(error.to_string(), "injected ConPTY reaper start failure");
		assert_eq!(operations.quarantine_len(), 1);
		assert_not_released(&released);

		let (next_process, next_raw, next_released) = tracked_handle();
		terminate_and_reap_with(next_process, operations.clone()).unwrap();
		operations.wait_for_reaper();
		assert_eq!(operations.spawn_calls(), 2);
		assert_eq!(operations.quarantine_len(), 0);
		assert_released(&next_released, next_raw);
		assert_not_released(&released);

		operations.signal_reapers();
		assert_released(&released, raw);
	}
}
