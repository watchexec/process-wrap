//! Exact `CreateProcessW` spawning for a prepared ConPTY command.
//!
//! # External premise
//!
//! This module accepts **TCB premise G2**: process creation with
//! `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` is an attachment to a new console for the null-standard-
//! handle replacement rule when `STARTF_USESTDHANDLES` is present. This premise reconciles the
//! [`CreateProcessW` contract](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw),
//! which requires valid standard-handle slots, with the
//! [`GetStdHandle` contract](https://learn.microsoft.com/en-us/windows/console/getstdhandle), which
//! describes replacement of null slots when attaching to a new console. The native standard-handle
//! regression checks deployed behavior but does not establish this platform contract.

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
	// aligned backing storage, and no native handles are inherited. Under TCB premise G2 above,
	// STARTF_USESTDHANDLES with three null slots causes ConPTY to install its console handles. The
	// process-information value is writable output storage.
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
	process: Option<ReapHandle>,
}

impl SpawnCleanup {
	pub(super) fn disarm(&mut self) {
		self.process.take();
	}

	pub(super) fn rollback(&mut self) -> io::Result<()> {
		self.process.take().map_or(Ok(()), |process| {
			terminate_and_reap_with(process, WindowsCleanupOperations)
		})
	}

	#[cfg(test)]
	fn tracked(process: OwnedHandle, released: std::sync::mpsc::Sender<usize>) -> Self {
		Self {
			process: Some(ReapHandle::tracked(process, released)),
		}
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
			process: Some(ReapHandle::new(duplicate(process)?)),
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

#[cfg(feature = "tracing")]
#[derive(Clone)]
struct CleanupDiagnostic {
	id: u64,
	action: &'static str,
	error: String,
	kind: io::ErrorKind,
	raw_os_error: Option<i32>,
	dispatch: tracing::Dispatch,
}

#[cfg(feature = "tracing")]
#[derive(Default)]
struct DiagnosticState {
	// At most one of `emitting` and `queued` is true. `queued` reserves exactly one shared-state
	// clone for the retry queue, including the short interval before that clone is inserted.
	next_id: u64,
	pending: Vec<CleanupDiagnostic>,
	dispatch: Option<tracing::Dispatch>,
	emitting: bool,
	queued: bool,
}

#[derive(Clone, Default)]
struct PendingDiagnostics {
	#[cfg(feature = "tracing")]
	state: Arc<Mutex<DiagnosticState>>,
}

impl PendingDiagnostics {
	fn bind_current_dispatch(&self) {
		#[cfg(feature = "tracing")]
		{
			let dispatch = tracing::dispatcher::get_default(Clone::clone);
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			state.dispatch.get_or_insert(dispatch);
		}
	}

	fn push(&self, action: &'static str, error: &io::Error) {
		#[cfg(feature = "tracing")]
		{
			let fallback_dispatch = tracing::dispatcher::get_default(Clone::clone);
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			let dispatch = state.dispatch.get_or_insert(fallback_dispatch).clone();
			let id = state.next_id;
			state.next_id = state.next_id.wrapping_add(1);
			state.pending.push(CleanupDiagnostic {
				id,
				action,
				error: error.to_string(),
				kind: error.kind(),
				raw_os_error: error.raw_os_error(),
				dispatch,
			});
		}
		#[cfg(not(feature = "tracing"))]
		let _ = (action, error);
	}

	fn try_begin_emit(&self, queued_retry: bool) -> bool {
		#[cfg(feature = "tracing")]
		{
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			if queued_retry {
				if !state.queued {
					return false;
				}
				debug_assert!(!state.emitting);
				state.queued = false;
			} else if state.queued {
				return false;
			}
			if state.pending.is_empty() || state.emitting {
				return false;
			}
			state.emitting = true;
			true
		}
		#[cfg(not(feature = "tracing"))]
		{
			let _ = queued_retry;
			false
		}
	}

	fn emission_start_failed(&self) -> bool {
		#[cfg(feature = "tracing")]
		{
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			debug_assert!(state.emitting);
			debug_assert!(!state.queued);
			state.emitting = false;
			if state.pending.is_empty() || state.queued {
				false
			} else {
				state.queued = true;
				true
			}
		}
		#[cfg(not(feature = "tracing"))]
		{
			false
		}
	}

	/// Emit one snapshot. Returns true after atomically reserving a later retry for entries whose
	/// diagnostic emission hit an unwinding panic or that arrived while this emitter was active.
	fn emit(&self) -> bool {
		#[cfg(feature = "tracing")]
		{
			let pending = {
				let state = self
					.state
					.lock()
					.unwrap_or_else(std::sync::PoisonError::into_inner);
				debug_assert!(state.emitting);
				debug_assert!(!state.queued);
				state.pending.clone()
			};
			let mut emitted = Vec::with_capacity(pending.len());
			for diagnostic in pending {
				let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
					tracing::dispatcher::with_default(&diagnostic.dispatch, || {
						tracing::warn!(
							error = %diagnostic.error,
							error_kind = ?diagnostic.kind,
							raw_os_error = ?diagnostic.raw_os_error,
							action = diagnostic.action,
							"secondary ConPTY cleanup failure"
						);
					});
				}));
				if result.is_ok() {
					emitted.push(diagnostic.id);
				}
			}
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			if !emitted.is_empty() {
				state
					.pending
					.retain(|diagnostic| !emitted.contains(&diagnostic.id));
			}
			debug_assert!(state.emitting);
			debug_assert!(!state.queued);
			state.emitting = false;
			if state.pending.is_empty() || state.queued {
				false
			} else {
				state.queued = true;
				true
			}
		}
		#[cfg(not(feature = "tracing"))]
		{
			false
		}
	}

	#[cfg(all(test, feature = "tracing"))]
	fn len(&self) -> usize {
		self.state
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.pending
			.len()
	}
}

/// Sole owner of the exact handle selected for rollback cleanup.
///
/// `process` is `Some` from construction until `Drop`; moving this value transfers that ownership
/// unchanged between the caller, reaper handoff cell, reaper thread, and quarantine. Only `Drop`
/// takes and closes the handle. Pending diagnostics move with this owner or share their state with
/// an off-caller emitter and retry queue only after the handle has reached a durable owner.
struct ReapHandle {
	process: Option<OwnedHandle>,
	diagnostics: PendingDiagnostics,
	#[cfg(test)]
	released: Option<std::sync::mpsc::Sender<usize>>,
}

impl std::fmt::Debug for ReapHandle {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("ReapHandle")
			.field("process", &self.process)
			.finish_non_exhaustive()
	}
}

impl ReapHandle {
	fn new(process: OwnedHandle) -> Self {
		Self {
			process: Some(process),
			diagnostics: PendingDiagnostics::default(),
			#[cfg(test)]
			released: None,
		}
	}

	#[cfg(test)]
	fn tracked(process: OwnedHandle, released: std::sync::mpsc::Sender<usize>) -> Self {
		Self {
			process: Some(process),
			diagnostics: PendingDiagnostics::default(),
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

	fn add_diagnostic(&self, action: &'static str, error: &io::Error) {
		self.diagnostics.push(action, error);
	}

	fn pending_diagnostics(&self) -> PendingDiagnostics {
		self.diagnostics.clone()
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

	/// Start an off-caller diagnostic task. Returning an error means the task was not started.
	fn spawn_diagnostic(&self, task: Box<dyn FnOnce() + Send>) -> io::Result<()>;

	fn queue_diagnostics(&self, diagnostics: PendingDiagnostics);
	fn take_queued_diagnostics(&self) -> Vec<PendingDiagnostics>;
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

	fn spawn_diagnostic(&self, task: Box<dyn FnOnce() + Send>) -> io::Result<()> {
		std::thread::Builder::new()
			.name("process-wrap-conpty-diagnostic".into())
			.spawn(task)
			.map(drop)
	}

	fn queue_diagnostics(&self, diagnostics: PendingDiagnostics) {
		queued_diagnostics()
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.push(diagnostics);
	}

	fn take_queued_diagnostics(&self) -> Vec<PendingDiagnostics> {
		std::mem::take(
			&mut *queued_diagnostics()
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner),
		)
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

fn queued_diagnostics() -> &'static Mutex<Vec<PendingDiagnostics>> {
	static QUEUED: OnceLock<Mutex<Vec<PendingDiagnostics>>> = OnceLock::new();
	QUEUED.get_or_init(|| Mutex::new(Vec::new()))
}

fn quarantined_processes() -> &'static Mutex<Vec<ReapHandle>> {
	static QUARANTINED: OnceLock<Mutex<Vec<ReapHandle>>> = OnceLock::new();
	QUARANTINED.get_or_init(|| Mutex::new(Vec::new()))
}

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
	process.diagnostics.bind_current_dispatch();
	retry_queued_diagnostics(operations.clone());
	let retry_error = retry_quarantined(operations.clone());
	let termination_error = operations.terminate(process.raw()).err();

	let current_error = match operations.wait(process.raw(), 0) {
		Ok(WaitOutcome::Signaled) => None,
		Ok(WaitOutcome::Timeout) => {
			if let Some(error) = termination_error.as_ref() {
				process.add_diagnostic(
					"failed to terminate an unsignaled ConPTY rollback process",
					error,
				);
			}
			let scheduling_error = defer_reap(process, operations.clone()).err();
			termination_error.or(scheduling_error)
		}
		Err(wait_error) => {
			if let Some(error) = termination_error.as_ref() {
				process.add_diagnostic("failed to terminate a ConPTY rollback process", error);
			}
			process.add_diagnostic("failed to probe a ConPTY rollback process", &wait_error);
			let _ = defer_reap(process, operations.clone());
			Some(wait_error)
		}
	};

	current_error.or(retry_error).map_or(Ok(()), Err)
}

fn retry_quarantined<O: CleanupOperations>(operations: O) -> Option<io::Error> {
	let mut first_error = None;
	for process in operations.take_quarantined() {
		let diagnostics = process.pending_diagnostics();
		match start_reaper(process, operations.clone()) {
			Ok(()) => schedule_diagnostics(diagnostics, operations.clone()),
			Err(failure) => {
				let ReaperStartError { error, process } = failure;
				process.add_diagnostic(
					"failed to retry a quarantined ConPTY rollback process",
					&error,
				);
				let diagnostics = process.pending_diagnostics();
				operations.quarantine(process);
				schedule_diagnostics(diagnostics, operations.clone());
				if first_error.is_none() {
					first_error = Some(error);
				}
			}
		}
	}
	first_error
}

fn defer_reap<O: CleanupOperations>(process: ReapHandle, operations: O) -> io::Result<()> {
	let diagnostics = process.pending_diagnostics();
	match start_reaper(process, operations.clone()) {
		Ok(()) => {
			schedule_diagnostics(diagnostics, operations);
			Ok(())
		}
		Err(failure) => {
			let ReaperStartError { error, process } = failure;
			process.add_diagnostic(
				"failed to start a ConPTY rollback reaper; quarantining the exact process handle",
				&error,
			);
			let diagnostics = process.pending_diagnostics();
			operations.quarantine(process);
			schedule_diagnostics(diagnostics, operations);
			Err(error)
		}
	}
}

fn retry_queued_diagnostics<O: CleanupOperations>(operations: O) {
	for diagnostics in operations.take_queued_diagnostics() {
		if diagnostics.try_begin_emit(true) {
			start_diagnostic_worker(diagnostics, operations.clone());
		}
	}
}

fn schedule_diagnostics<O: CleanupOperations>(diagnostics: PendingDiagnostics, operations: O) {
	if diagnostics.try_begin_emit(false) {
		start_diagnostic_worker(diagnostics, operations);
	}
}

fn start_diagnostic_worker<O: CleanupOperations>(diagnostics: PendingDiagnostics, operations: O) {
	let task_diagnostics = diagnostics.clone();
	let task_operations = operations.clone();
	let task = Box::new(move || {
		if task_diagnostics.emit() {
			task_operations.queue_diagnostics(task_diagnostics);
		}
	});
	if operations.spawn_diagnostic(task).is_err() && diagnostics.emission_start_failed() {
		// Diagnosing this recursively could repeat the same thread-creation failure. The state was
		// atomically returned to queued/non-emitting and is retried by a later cleanup opportunity.
		operations.queue_diagnostics(diagnostics);
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
				process.add_diagnostic(
					"ConPTY rollback reaper could not establish process completion; quarantining the exact process handle",
					&error,
				);
				let diagnostics = process.pending_diagnostics();
				task_operations.quarantine(process);
				schedule_diagnostics(diagnostics, task_operations.clone());
			}
			Err(error) => {
				process.add_diagnostic(
					"ConPTY rollback reaper wait failed; quarantining the exact process handle",
					&error,
				);
				let diagnostics = process.pending_diagnostics();
				task_operations.quarantine(process);
				schedule_diagnostics(diagnostics, task_operations.clone());
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

fn win32_io_error(error: windows::core::Error) -> io::Error {
	WIN32_ERROR::from_error(&error)
		.and_then(|error| i32::try_from(error.0).ok())
		.map_or_else(|| io::Error::other(error), io::Error::from_raw_os_error)
}

#[cfg(test)]
mod tests {
	use std::{
		collections::VecDeque,
		io,
		os::windows::io::{AsRawHandle, FromRawHandle},
		panic::{AssertUnwindSafe, catch_unwind, panic_any},
		sync::{
			Arc, Condvar, Mutex,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		thread::ThreadId,
		time::Duration,
	};

	#[cfg(feature = "tracing")]
	use tracing::{
		Event, Metadata, Subscriber,
		span::{Attributes, Id, Record},
	};
	use windows::{
		Win32::System::{
			Console::HPCON,
			Threading::{CreateEventW, STARTF_USESTDHANDLES, SetEvent},
		},
		core::PCWSTR,
	};

	use crate::{
		SpawnTransaction,
		tokio::{
			ChildWrapper, Command, CommandWrapper, ProviderProduct, SpawnAttempt, SpawnProvider,
		},
	};

	use super::*;

	#[derive(Clone, Copy, Debug)]
	enum LifecycleFailure {
		Error,
		Panic,
	}

	#[derive(Debug)]
	struct FinalizationFailureChild(LifecycleFailure);

	impl ChildWrapper for FinalizationFailureChild {
		fn inner(&self) -> &dyn ChildWrapper {
			self
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self
		}

		fn finalize_spawn_layer(&mut self) -> io::Result<()> {
			match self.0 {
				LifecycleFailure::Error => Err(io::Error::other("finalization failed")),
				LifecycleFailure::Panic => panic_any("finalization failed"),
			}
		}
	}

	#[derive(Debug)]
	struct TrackedCleanupTransaction(SpawnCleanup);

	impl SpawnTransaction for TrackedCleanupTransaction {
		fn commit(&mut self) -> io::Result<()> {
			self.0.disarm();
			Ok(())
		}

		fn rollback(&mut self) -> io::Result<()> {
			self.0.rollback()
		}
	}

	struct CleanupControl {
		signal: OwnedHandle,
		released: mpsc::Receiver<usize>,
		expected_handle: usize,
	}

	#[derive(Debug)]
	struct TrackedCleanupProvider {
		failure: LifecycleFailure,
		control: mpsc::Sender<CleanupControl>,
	}

	impl SpawnProvider for TrackedCleanupProvider {
		fn spawn(
			&self,
			_attempt: &mut SpawnAttempt,
			_command: &Command,
		) -> io::Result<ProviderProduct> {
			// SAFETY: default security, manual reset, initially unsignaled, and no name request an event
			// handle which is transferred immediately to `OwnedHandle`.
			let event = unsafe { CreateEventW(None, true, false, None) }?;
			// SAFETY: the successful CreateEventW result is uniquely owned here.
			let process = unsafe { OwnedHandle::from_raw_handle(event.0) };
			let signal = duplicate(&process)?;
			let expected_handle = process.as_raw_handle() as usize;
			let (released, released_rx) = mpsc::channel();
			let cleanup = SpawnCleanup::tracked(process, released);
			self.control
				.send(CleanupControl {
					signal,
					released: released_rx,
					expected_handle,
				})
				.expect("the lifecycle test still receives cleanup ownership");
			Ok(ProviderProduct::new(
				Box::new(FinalizationFailureChild(self.failure)),
				Box::new(TrackedCleanupTransaction(cleanup)),
			))
		}
	}

	#[derive(Debug)]
	struct TrackedCleanupWrapper(TrackedCleanupProvider);

	impl CommandWrapper for TrackedCleanupWrapper {
		fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
			Some(&self.0)
		}
	}

	#[test]
	fn finalization_failures_transfer_the_exact_transaction_handle_to_the_reaper() -> io::Result<()>
	{
		for failure in [LifecycleFailure::Error, LifecycleFailure::Panic] {
			let (control, control_rx) = mpsc::channel();
			let mut command = Command::new("provider-owned-program");
			command.wrap(TrackedCleanupWrapper(TrackedCleanupProvider {
				failure,
				control,
			}));

			match failure {
				LifecycleFailure::Error => {
					let error = command.spawn().expect_err("finalization must fail");
					assert_eq!(error.to_string(), "finalization failed");
				}
				LifecycleFailure::Panic => {
					let payload = catch_unwind(AssertUnwindSafe(|| command.spawn()))
						.expect_err("finalization must panic");
					assert_eq!(
						*payload
							.downcast::<&'static str>()
							.expect("the original panic payload is preserved"),
						"finalization failed"
					);
				}
			}

			let control = control_rx
				.recv_timeout(Duration::from_secs(2))
				.expect("the provider reports its exact cleanup handle");
			assert_eq!(control.released.try_recv(), Err(mpsc::TryRecvError::Empty));
			// SAFETY: `signal` owns a live duplicate of the manual-reset event waited on by the reaper.
			unsafe { SetEvent(HANDLE(control.signal.as_raw_handle())) }?;
			assert_eq!(
				control
					.released
					.recv_timeout(Duration::from_secs(2))
					.expect("the reaper releases the exact handle after completion"),
				control.expected_handle
			);
		}
		Ok(())
	}

	#[cfg(feature = "tracing")]
	#[derive(Clone, Copy, Debug)]
	enum SubscriberBehavior {
		Block,
		Panic,
		Record,
	}

	#[cfg(feature = "tracing")]
	#[derive(Debug, Default)]
	struct SubscriberGate {
		events: usize,
		released: bool,
	}

	#[cfg(feature = "tracing")]
	#[derive(Debug)]
	struct SubscriberState {
		behavior: SubscriberBehavior,
		gate: Mutex<SubscriberGate>,
		changed: Condvar,
	}

	#[cfg(feature = "tracing")]
	impl SubscriberState {
		fn new(behavior: SubscriberBehavior) -> Arc<Self> {
			Arc::new(Self {
				behavior,
				gate: Mutex::new(SubscriberGate::default()),
				changed: Condvar::new(),
			})
		}

		fn wait_for_event(&self) {
			self.wait_for_events(1);
		}

		fn wait_for_events(&self, count: usize) {
			assert!(
				self.observe_events(count),
				"cleanup diagnostic was not emitted"
			);
		}

		fn observe_events(&self, count: usize) -> bool {
			let deadline = std::time::Instant::now() + Duration::from_secs(2);
			while self.event_count() < count {
				if std::time::Instant::now() >= deadline {
					return false;
				}
				std::thread::sleep(Duration::from_millis(1));
			}
			true
		}

		fn event_count(&self) -> usize {
			self.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.events
		}

		fn release(&self) {
			let mut gate = self
				.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			gate.released = true;
			self.changed.notify_all();
		}
	}

	#[cfg(feature = "tracing")]
	#[derive(Debug)]
	struct AdversarialSubscriber {
		state: Arc<SubscriberState>,
	}

	#[cfg(feature = "tracing")]
	impl Subscriber for AdversarialSubscriber {
		fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
			true
		}

		fn new_span(&self, _span: &Attributes<'_>) -> Id {
			Id::from_u64(1)
		}

		fn record(&self, _span: &Id, _values: &Record<'_>) {}

		fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

		fn event(&self, _event: &Event<'_>) {
			let mut gate = self
				.state
				.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			gate.events += 1;
			self.state.changed.notify_all();
			match self.state.behavior {
				SubscriberBehavior::Block => {
					while !gate.released {
						gate = self.state.changed.wait(gate).unwrap();
					}
				}
				SubscriberBehavior::Panic => panic!("injected cleanup diagnostic panic"),
				SubscriberBehavior::Record => {}
			}
		}

		fn enter(&self, _span: &Id) {}

		fn exit(&self, _span: &Id) {}
	}

	#[cfg(feature = "tracing")]
	fn diagnostic_dispatch(state: Arc<SubscriberState>) -> tracing::Dispatch {
		tracing::Dispatch::new(AdversarialSubscriber { state })
	}

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

	struct TestState {
		terminate_error: Option<i32>,
		probes: Mutex<VecDeque<WaitPlan>>,
		reaper_results: Mutex<VecDeque<WaitPlan>>,
		spawn_failures: AtomicUsize,
		spawn_calls: AtomicUsize,
		diagnostic_spawn_failures: AtomicUsize,
		diagnostic_spawn_calls: AtomicUsize,
		active_diagnostic_threads: AtomicUsize,
		max_active_diagnostic_threads: AtomicUsize,
		waits: Mutex<Vec<WaitRecord>>,
		gate: Mutex<ReaperGate>,
		gate_changed: Condvar,
		queued_diagnostics: Mutex<Vec<PendingDiagnostics>>,
		quarantine: Mutex<Vec<ReapHandle>>,
	}

	#[derive(Clone)]
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
					reaper_results: Mutex::new(VecDeque::new()),
					spawn_failures: AtomicUsize::new(spawn_failures),
					spawn_calls: AtomicUsize::new(0),
					diagnostic_spawn_failures: AtomicUsize::new(0),
					diagnostic_spawn_calls: AtomicUsize::new(0),
					active_diagnostic_threads: AtomicUsize::new(0),
					max_active_diagnostic_threads: AtomicUsize::new(0),
					waits: Mutex::new(Vec::new()),
					gate: Mutex::new(ReaperGate::default()),
					gate_changed: Condvar::new(),
					queued_diagnostics: Mutex::new(Vec::new()),
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

		#[cfg(feature = "tracing")]
		fn set_reaper_results(&self, results: impl IntoIterator<Item = WaitPlan>) {
			self.state.reaper_results.lock().unwrap().extend(results);
		}

		fn quarantine_len(&self) -> usize {
			self.state.quarantine.lock().unwrap().len()
		}

		#[cfg(feature = "tracing")]
		fn queued_diagnostics_len(&self) -> usize {
			self.state.queued_diagnostics.lock().unwrap().len()
		}

		#[cfg(feature = "tracing")]
		fn quarantined_diagnostic_count(&self) -> usize {
			self.state
				.quarantine
				.lock()
				.unwrap()
				.iter()
				.map(|process| process.diagnostics.len())
				.sum()
		}

		fn spawn_calls(&self) -> usize {
			self.state.spawn_calls.load(Ordering::SeqCst)
		}

		#[cfg(feature = "tracing")]
		fn set_diagnostic_spawn_failures(&self, failures: usize) {
			self.state
				.diagnostic_spawn_failures
				.store(failures, Ordering::SeqCst);
		}

		#[cfg(feature = "tracing")]
		fn diagnostic_spawn_calls(&self) -> usize {
			self.state.diagnostic_spawn_calls.load(Ordering::SeqCst)
		}

		#[cfg(feature = "tracing")]
		fn active_diagnostic_threads(&self) -> usize {
			self.state.active_diagnostic_threads.load(Ordering::SeqCst)
		}

		#[cfg(feature = "tracing")]
		fn wait_for_no_diagnostic_threads(&self) {
			let deadline = std::time::Instant::now() + Duration::from_secs(2);
			while self.active_diagnostic_threads() != 0 {
				assert!(
					std::time::Instant::now() < deadline,
					"diagnostic emitters did not finish"
				);
				std::thread::sleep(Duration::from_millis(1));
			}
		}

		#[cfg(feature = "tracing")]
		fn max_active_diagnostic_threads(&self) -> usize {
			self.state
				.max_active_diagnostic_threads
				.load(Ordering::SeqCst)
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
			drop(gate);
			self.state
				.reaper_results
				.lock()
				.unwrap()
				.pop_front()
				.unwrap_or(WaitPlan::Signaled)
				.result()
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

		fn spawn_diagnostic(&self, task: Box<dyn FnOnce() + Send>) -> io::Result<()> {
			self.state
				.diagnostic_spawn_calls
				.fetch_add(1, Ordering::SeqCst);
			let fail = self
				.state
				.diagnostic_spawn_failures
				.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
					remaining.checked_sub(1)
				})
				.is_ok();
			if fail {
				return Err(io::Error::other(
					"injected ConPTY diagnostic thread start failure",
				));
			}
			let state = Arc::clone(&self.state);
			std::thread::Builder::new()
				.name("test-process-wrap-conpty-diagnostic".into())
				.spawn(move || {
					let active = state
						.active_diagnostic_threads
						.fetch_add(1, Ordering::SeqCst)
						+ 1;
					state
						.max_active_diagnostic_threads
						.fetch_max(active, Ordering::SeqCst);
					task();
					state
						.active_diagnostic_threads
						.fetch_sub(1, Ordering::SeqCst);
				})
				.map(drop)
		}

		fn queue_diagnostics(&self, diagnostics: PendingDiagnostics) {
			self.state
				.queued_diagnostics
				.lock()
				.unwrap()
				.push(diagnostics);
		}

		fn take_queued_diagnostics(&self) -> Vec<PendingDiagnostics> {
			std::mem::take(&mut *self.state.queued_diagnostics.lock().unwrap())
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

	#[cfg(feature = "tracing")]
	#[test]
	fn blocking_diagnostic_subscriber_runs_before_signal_without_blocking_cleanup_or_reaping() {
		let operations = TestOperations::new(Some(5), [WaitPlan::Timeout], 0);
		let (process, raw, released) = tracked_handle();
		let subscriber = SubscriberState::new(SubscriberBehavior::Block);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));
		let (completed, observe_completion) = mpsc::channel();
		let caller_operations = operations.clone();
		let caller = std::thread::spawn(move || {
			let result = tracing::dispatcher::with_default(&dispatch, || {
				terminate_and_reap_with(process, caller_operations)
			});
			completed.send(result).unwrap();
		});

		let completion = observe_completion.recv_timeout(Duration::from_secs(2));
		if completion.is_err() {
			subscriber.release();
			let _ = observe_completion.recv_timeout(Duration::from_secs(2));
			operations.wait_for_reaper();
			operations.signal_reapers();
			let _ = released.recv_timeout(Duration::from_secs(2));
			caller.join().unwrap();
			panic!("cleanup dispatched a blocking diagnostic on its caller thread");
		}
		assert_eq!(completion.unwrap().unwrap_err().raw_os_error(), Some(5));
		caller.join().unwrap();
		operations.wait_for_reaper();
		subscriber.wait_for_event();
		assert_eq!(operations.diagnostic_spawn_calls(), 1);
		assert_eq!(operations.max_active_diagnostic_threads(), 1);
		assert_eq!(subscriber.event_count(), 1);
		assert_not_released(&released);

		operations.signal_reapers();
		assert_released(&released, raw);
		subscriber.release();
		operations.wait_for_no_diagnostic_threads();
		assert_eq!(subscriber.event_count(), 1);
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn repeated_start_failures_keep_one_blocked_diagnostic_emitter() {
		let operations = TestOperations::new(
			None,
			[WaitPlan::Timeout, WaitPlan::Signaled, WaitPlan::Signaled],
			2,
		);
		let subscriber = SubscriberState::new(SubscriberBehavior::Block);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));
		let (process, raw, released) = tracked_handle();

		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(process, operations.clone())
		})
		.unwrap_err();
		subscriber.wait_for_event();
		assert_eq!(operations.quarantine_len(), 1);
		assert_not_released(&released);

		let (second, second_raw, second_released) = tracked_handle();
		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(second, operations.clone())
		})
		.unwrap_err();
		assert_released(&second_released, second_raw);
		let spawn_calls_while_blocked = operations.diagnostic_spawn_calls();
		let max_active_while_blocked = operations.max_active_diagnostic_threads();
		assert_eq!(operations.quarantine_len(), 1);
		assert_not_released(&released);

		subscriber.release();
		operations.wait_for_no_diagnostic_threads();

		let (third, third_raw, third_released) = tracked_handle();
		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(third, operations.clone())
		})
		.unwrap();
		operations.wait_for_reaper();
		subscriber.wait_for_events(2);
		assert_released(&third_released, third_raw);

		operations.signal_reapers();
		assert_released(&released, raw);
		assert_eq!(spawn_calls_while_blocked, 1);
		assert_eq!(max_active_while_blocked, 1);
		assert_eq!(operations.max_active_diagnostic_threads(), 1);
		assert_eq!(subscriber.event_count(), 2);
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn diagnostic_start_failure_retries_once_while_reaper_remains_unsignaled() {
		let operations = TestOperations::new(Some(5), [WaitPlan::Timeout, WaitPlan::Signaled], 0);
		operations.set_diagnostic_spawn_failures(1);
		let subscriber = SubscriberState::new(SubscriberBehavior::Record);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));
		let (process, raw, released) = tracked_handle();

		let error = tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(process, operations.clone())
		})
		.unwrap_err();
		assert_eq!(error.raw_os_error(), Some(5));
		operations.wait_for_reaper();
		assert_eq!(operations.diagnostic_spawn_calls(), 1);
		assert_eq!(operations.queued_diagnostics_len(), 1);
		assert_eq!(subscriber.event_count(), 0);
		assert_not_released(&released);

		let (next, next_raw, next_released) = tracked_handle();
		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(next, operations.clone())
		})
		.unwrap();
		assert_released(&next_released, next_raw);
		let emitted_before_signal = subscriber.observe_events(1);
		operations.wait_for_no_diagnostic_threads();
		assert_not_released(&released);

		operations.signal_reapers();
		assert_released(&released, raw);
		assert!(
			emitted_before_signal,
			"a later cleanup did not retry the retained diagnostic"
		);
		assert_eq!(operations.diagnostic_spawn_calls(), 2);
		assert_eq!(operations.queued_diagnostics_len(), 0);
		assert_eq!(operations.max_active_diagnostic_threads(), 1);
		assert_eq!(subscriber.event_count(), 1);
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn panicking_diagnostic_subscriber_cannot_replace_the_primary_error() {
		let operations = TestOperations::new(Some(5), [WaitPlan::Timeout], 0);
		let (process, raw, released) = tracked_handle();
		let subscriber = SubscriberState::new(SubscriberBehavior::Panic);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));

		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			tracing::dispatcher::with_default(&dispatch, || {
				let _ = terminate_and_reap_with(process, operations.clone());
				Err::<(), _>(io::Error::other("primary lifecycle error"))
			})
		}));
		let error = result
			.expect("cleanup diagnostics must not unwind through the caller")
			.unwrap_err();
		assert_eq!(error.to_string(), "primary lifecycle error");
		operations.wait_for_reaper();
		assert_not_released(&released);

		operations.signal_reapers();
		subscriber.wait_for_event();
		assert_released(&released, raw);
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn panicking_diagnostic_subscriber_cannot_replace_the_primary_panic() {
		let operations = TestOperations::new(Some(5), [WaitPlan::Timeout], 0);
		let (process, raw, released) = tracked_handle();
		let subscriber = SubscriberState::new(SubscriberBehavior::Panic);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));

		let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			tracing::dispatcher::with_default(&dispatch, || {
				let _ = terminate_and_reap_with(process, operations.clone());
				panic!("primary lifecycle panic");
			})
		}))
		.expect_err("the primary lifecycle panic must be resumed");
		assert_eq!(
			*panic.downcast::<&'static str>().unwrap(),
			"primary lifecycle panic"
		);
		operations.wait_for_reaper();
		assert_not_released(&released);

		operations.signal_reapers();
		subscriber.wait_for_event();
		assert_released(&released, raw);
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn panicking_diagnostics_leave_start_and_retry_failures_with_quarantined_handle() {
		let operations = TestOperations::new(
			None,
			[WaitPlan::Timeout, WaitPlan::Signaled, WaitPlan::Signaled],
			2,
		);
		let subscriber = SubscriberState::new(SubscriberBehavior::Panic);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));
		let (process, raw, released) = tracked_handle();

		let error = tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(process, operations.clone())
		})
		.unwrap_err();
		assert_eq!(error.to_string(), "injected ConPTY reaper start failure");
		subscriber.wait_for_events(1);
		operations.wait_for_no_diagnostic_threads();
		assert_eq!(operations.queued_diagnostics_len(), 1);
		assert_eq!(operations.quarantine_len(), 1);
		assert_eq!(operations.quarantined_diagnostic_count(), 1);
		assert_not_released(&released);

		let (second, second_raw, second_released) = tracked_handle();
		let retry_error = tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(second, operations.clone())
		})
		.unwrap_err();
		assert_eq!(
			retry_error.to_string(),
			"injected ConPTY reaper start failure"
		);
		subscriber.wait_for_events(2);
		operations.wait_for_no_diagnostic_threads();
		let events_after_second_attempt = subscriber.event_count();
		assert!(events_after_second_attempt <= 3);
		assert_eq!(operations.queued_diagnostics_len(), 1);
		assert_eq!(operations.quarantine_len(), 1);
		assert_eq!(operations.quarantined_diagnostic_count(), 2);
		assert_released(&second_released, second_raw);
		assert_not_released(&released);

		let (third, third_raw, third_released) = tracked_handle();
		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(third, operations.clone())
		})
		.unwrap();
		operations.wait_for_reaper();
		subscriber.wait_for_events(events_after_second_attempt + 2);
		operations.wait_for_no_diagnostic_threads();
		assert_eq!(operations.quarantine_len(), 0);
		assert_released(&third_released, third_raw);
		assert_not_released(&released);

		operations.signal_reapers();
		assert_released(&released, raw);
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn panicking_diagnostic_preserves_a_handle_after_reaper_wait_failure() {
		let operations = TestOperations::new(None, [WaitPlan::Timeout, WaitPlan::Signaled], 0);
		operations.set_reaper_results([WaitPlan::Error(6), WaitPlan::Signaled]);
		let subscriber = SubscriberState::new(SubscriberBehavior::Panic);
		let dispatch = diagnostic_dispatch(Arc::clone(&subscriber));
		let (process, raw, released) = tracked_handle();

		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(process, operations.clone())
		})
		.unwrap();
		operations.wait_for_reaper();
		operations.signal_reapers();
		subscriber.wait_for_events(1);
		operations.wait_for_no_diagnostic_threads();
		assert_eq!(operations.queued_diagnostics_len(), 1);
		assert_eq!(operations.quarantine_len(), 1);
		assert_eq!(operations.quarantined_diagnostic_count(), 1);
		assert_not_released(&released);

		let (next, next_raw, next_released) = tracked_handle();
		tracing::dispatcher::with_default(&dispatch, || {
			terminate_and_reap_with(next, operations.clone())
		})
		.unwrap();
		subscriber.wait_for_events(2);
		assert_eq!(operations.quarantine_len(), 0);
		assert_released(&next_released, next_raw);
		assert_released(&released, raw);
	}
}
