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
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Waker},
};

use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
#[cfg(feature = "job-object")]
use windows::Win32::System::Threading::ResumeThread;
use windows::Win32::{
	Foundation::{
		DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
	},
	System::Threading::{
		GetCurrentProcess, GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
	},
};

use crate::{ChildExitStatus, tokio::ChildWrapper};

use super::super::{ControllerSlot, PtyController};

#[derive(Debug)]
pub(super) struct ConPtyChild {
	process: OwnedHandle,
	primary_thread: Option<OwnedHandle>,
	pid: u32,
	kill_on_drop: bool,
	cleanup_armed: bool,
	controller: Option<Arc<ControllerSlot>>,
	exit_status: ChildExitStatus,
	wait_task: Option<WaitTask>,
	wait_cancel: Arc<AtomicBool>,
	stdin: Option<ChildStdin>,
	stdout: Option<ChildStdout>,
	stderr: Option<ChildStderr>,
}

#[derive(Debug)]
struct WaitTask {
	state: Arc<WaitState>,
	_thread: std::thread::JoinHandle<()>,
}

#[derive(Debug, Default)]
struct WaitState {
	result: Mutex<WaitResult>,
}

#[derive(Debug, Default)]
struct WaitResult {
	result: Option<io::Result<ExitStatus>>,
	waker: Option<Waker>,
}

impl WaitTask {
	fn spawn(process: OwnedHandle, cancel: Arc<AtomicBool>) -> io::Result<Self> {
		let state = Arc::new(WaitState::default());
		let thread_state = Arc::clone(&state);
		let thread = std::thread::Builder::new()
			.name("process-wrap-conpty-wait".into())
			.spawn(move || {
				let result = loop {
					if cancel.load(Ordering::Acquire) {
						break Err(io::Error::new(
							io::ErrorKind::Interrupted,
							"ConPTY process wait was canceled because the child was dropped",
						));
					}
					match process_status(HANDLE(process.as_raw_handle()), 50) {
						Ok(Some(status)) => break Ok(status),
						Ok(None) => {}
						Err(error) => break Err(error),
					}
				};
				thread_state.complete(result);
			})?;
		Ok(Self {
			state,
			_thread: thread,
		})
	}

	fn clear_waker(&self) {
		self.state
			.result
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.waker
			.take();
	}

	#[cfg(test)]
	fn id(&self) -> std::thread::ThreadId {
		self._thread.thread().id()
	}
}

impl Future for WaitTask {
	type Output = io::Result<ExitStatus>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let mut state = self
			.state
			.result
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if let Some(result) = state.result.take() {
			return Poll::Ready(result);
		}
		if state
			.waker
			.as_ref()
			.is_none_or(|waker| !waker.will_wake(cx.waker()))
		{
			state.waker = Some(cx.waker().clone());
		}
		Poll::Pending
	}
}

impl Drop for WaitTask {
	fn drop(&mut self) {
		self.clear_waker();
	}
}

impl WaitState {
	fn complete(&self, result: io::Result<ExitStatus>) {
		let waker = {
			let mut state = self
				.result
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			state.result = Some(result);
			state.waker.take()
		};
		if let Some(waker) = waker {
			waker.wake();
		}
	}
}

struct RegisteredWait<'a> {
	task: &'a mut WaitTask,
}

impl Future for RegisteredWait<'_> {
	type Output = io::Result<ExitStatus>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		Pin::new(&mut *self.task).poll(cx)
	}
}

impl Drop for RegisteredWait<'_> {
	fn drop(&mut self) {
		self.task.clear_waker();
	}
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
			cleanup_armed: true,
			controller: None,
			exit_status: ChildExitStatus::Running,
			wait_task: None,
			wait_cancel: Arc::new(AtomicBool::new(false)),
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

	pub(super) fn install_controller(&mut self, controller: Arc<ControllerSlot>) {
		debug_assert!(self.controller.is_none());
		self.controller = Some(controller);
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

	#[cfg(feature = "job-object")]
	fn resume_after_job_assignment(&mut self) -> Option<io::Result<()>> {
		Some(self.resume_primary_thread())
	}

	fn disarm_job_object_layer(&mut self) -> io::Result<()> {
		self.primary_thread.take();
		self.cleanup_armed = false;
		if let Some(controller) = &self.controller {
			controller.commit();
		}
		Ok(())
	}

	fn take_pty_controller_layer(&mut self) -> Option<PtyController> {
		self.controller
			.as_ref()
			.and_then(|controller| controller.take())
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
			self.wait_task.take();
		}
		Ok(status)
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
		Box::pin(async move {
			if let ChildExitStatus::Exited(status) = self.exit_status {
				return Ok(status);
			}

			if self.wait_task.is_none() {
				let process = duplicate_handle(&self.process)?;
				let cancel = Arc::clone(&self.wait_cancel);
				self.wait_task = Some(WaitTask::spawn(process, cancel)?);
			}
			let result = RegisteredWait {
				task: self
					.wait_task
					.as_mut()
					.expect("an in-progress ConPTY wait must retain its task"),
			}
			.await;
			self.wait_task.take();
			let status = result?;
			self.exit_status = ChildExitStatus::Exited(status);
			Ok(status)
		})
	}
}

impl Drop for ConPtyChild {
	fn drop(&mut self) {
		self.wait_cancel.store(true, Ordering::Release);
		if (self.cleanup_armed || self.kill_on_drop)
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
			cleanup_armed: false,
			controller: None,
			exit_status: ChildExitStatus::Running,
			wait_task: None,
			wait_cancel: Arc::new(AtomicBool::new(false)),
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
	async fn start_kill_succeeds_after_natural_exit() {
		let mut native = spawn_exit_259();
		let mut child = wrap(&native, false);
		let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(status.code(), Some(259));
		child.start_kill().unwrap();
		assert_eq!(child.try_wait().unwrap(), Some(status));
		assert_eq!(child.wait().await.unwrap(), status);
		assert_eq!(wait_native(&mut native), status);
	}

	#[tokio::test]
	async fn kill_succeeds_after_natural_exit() {
		let mut native = spawn_exit_259();
		let mut child = wrap(&native, false);
		let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(status.code(), Some(259));
		Pin::from(ChildWrapper::kill(&mut child)).await.unwrap();
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
	async fn canceled_async_waits_share_one_task_and_preserve_the_process_handle() {
		let mut native = spawn_long_running();
		let mut child = wrap(&native, false);
		assert!(
			tokio::time::timeout(Duration::from_millis(20), child.wait())
				.await
				.is_err()
		);
		let wait_id = child
			.wait_task
			.as_ref()
			.expect("a canceled wait must retain its task")
			.id();
		for _ in 0..7 {
			assert!(
				tokio::time::timeout(Duration::from_millis(20), child.wait())
					.await
					.is_err()
			);
			assert_eq!(
				child
					.wait_task
					.as_ref()
					.expect("a canceled wait must retain its task")
					.id(),
				wait_id
			);
		}
		child.start_kill().unwrap();
		let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
			.await
			.unwrap()
			.unwrap();
		assert_eq!(wait_native(&mut native), status);
	}

	#[test]
	fn canceled_wait_does_not_block_runtime_shutdown_while_child_lives() {
		let mut native = spawn_long_running();
		let mut child = Some(wrap(&native, false));
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.unwrap();
		runtime.block_on(async {
			assert!(
				tokio::time::timeout(Duration::from_millis(20), child.as_mut().unwrap().wait(),)
					.await
					.is_err()
			);
		});

		let (shutdown_finished, observe_shutdown) = std::sync::mpsc::channel();
		let shutdown = std::thread::spawn(move || {
			drop(runtime);
			let _ = shutdown_finished.send(());
		});
		let runtime_stopped = observe_shutdown
			.recv_timeout(Duration::from_secs(2))
			.is_ok();
		if !runtime_stopped {
			drop(child.take());
		}
		shutdown.join().unwrap();
		assert!(
			runtime_stopped,
			"dropping the runtime waited for the canceled child wait"
		);

		assert!(native.try_wait().unwrap().is_none());
		drop(child.take());
		native.kill().unwrap();
		let _ = wait_native(&mut native);
	}

	#[tokio::test]
	async fn dropping_after_a_canceled_wait_releases_the_waiter_thread() {
		let mut native = spawn_long_running();
		let mut child = wrap(&native, false);
		assert!(
			tokio::time::timeout(Duration::from_millis(20), child.wait())
				.await
				.is_err()
		);
		let task = child
			.wait_task
			.take()
			.expect("a canceled wait retains its blocking task");
		drop(child);
		let error = tokio::time::timeout(Duration::from_secs(5), task)
			.await
			.unwrap()
			.unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::Interrupted);
		assert!(native.try_wait().unwrap().is_none());
		native.kill().unwrap();
		let _ = wait_native(&mut native);
	}

	#[test]
	fn armed_spawn_cleanup_terminates_a_running_process() {
		let mut native = spawn_long_running();
		let mut child = wrap(&native, false);
		child.cleanup_armed = true;
		drop(child);
		let _ = wait_native(&mut native);
	}

	#[test]
	fn final_spawn_disarm_preserves_a_running_process() {
		let mut native = spawn_long_running();
		let mut child = wrap(&native, false);
		child.primary_thread = Some(duplicate_process(&native));
		child.cleanup_armed = true;
		child.disarm_job_object_layer().unwrap();
		assert!(child.primary_thread_handle().is_none());
		drop(child);
		assert!(native.try_wait().unwrap().is_none());
		native.kill().unwrap();
		let _ = wait_native(&mut native);
	}

	#[test]
	fn kill_on_drop_terminates_a_running_process() {
		let mut native = spawn_long_running();
		let child = wrap(&native, true);
		drop(child);
		let _ = wait_native(&mut native);
	}
}
