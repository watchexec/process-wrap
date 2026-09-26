use std::{
	any::Any,
	future::Future,
	io::{Error, ErrorKind, Result},
	os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle},
	pin::Pin,
	process::ExitStatus,
	time::Duration,
};

use tokio::task::spawn_blocking;
#[cfg(feature = "tracing")]
use tracing::{debug, instrument};
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE},
	System::Threading::PROCESS_CREATION_FLAGS,
};

use crate::{
	ChildExitStatus,
	windows::{
		JobPort, job_creation_flags, make_job_object, resume_threads, set_job_kill_on_drop,
		terminate_job, wait_on_job,
	},
};

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingWaitEvent {
	Begin { handle: usize, owned: bool },
	End { handle: usize },
	Dropped { handle: usize },
}

#[cfg(test)]
fn blocking_wait_observer()
-> &'static std::sync::Mutex<Option<std::sync::mpsc::Sender<BlockingWaitEvent>>> {
	static OBSERVER: std::sync::OnceLock<
		std::sync::Mutex<Option<std::sync::mpsc::Sender<BlockingWaitEvent>>>,
	> = std::sync::OnceLock::new();
	OBSERVER.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn observe_blocking_wait(event: BlockingWaitEvent) {
	if let Some(observer) = blocking_wait_observer().lock().unwrap().as_ref() {
		let _ = observer.send(event);
	}
}

fn blocking_wait_on_job(completion_port: OwnedHandle) -> Result<std::ops::ControlFlow<()>> {
	#[cfg(test)]
	let handle = completion_port.as_raw_handle() as usize;
	#[cfg(test)]
	observe_blocking_wait(BlockingWaitEvent::Begin {
		handle,
		owned: true,
	});
	let result = wait_on_job(completion_port.as_handle(), None);
	#[cfg(test)]
	observe_blocking_wait(BlockingWaitEvent::End { handle });
	drop(completion_port);
	#[cfg(test)]
	observe_blocking_wait(BlockingWaitEvent::Dropped { handle });
	result
}

#[cfg(feature = "creation-flags")]
use super::CreationFlags;
#[cfg(feature = "kill-on-drop")]
use super::KillOnDrop;
use super::{ChildWrapper, CommandWrap, CommandWrapper, SpawnAttempt};

/// Wrapper which creates a job object context for a `Command`.
///
/// This wrapper is only available on Windows.
///
/// It creates a Windows Job Object and associates the [`Command`](super::Command) to it. This behaves analogously
/// to process groups on Unix or even cgroups on Linux, with the ability to restrict resource use.
/// See [Job Objects](https://docs.microsoft.com/en-us/windows/win32/procthread/job-objects).
///
/// This wrapper provides a child wrapper: [`JobObjectChild`].
///
/// [`CreationFlags`] may be registered before or after `JobObject`; process-wrap preserves its flags
/// and distinguishes explicit suspension from the temporary suspension needed for assignment.
#[derive(Clone, Copy, Debug)]
pub struct JobObject;

fn user_creation_flags(core: &CommandWrap) -> PROCESS_CREATION_FLAGS {
	#[cfg(feature = "creation-flags")]
	{
		core.get_wrap::<CreationFlags>()
			.map_or(PROCESS_CREATION_FLAGS(0), |flags| flags.0)
	}
	#[cfg(not(feature = "creation-flags"))]
	{
		let _ = core;
		PROCESS_CREATION_FLAGS(0)
	}
}

fn terminate_child(child: &mut dyn ChildWrapper) {
	let _ = child.start_kill();
}

#[derive(Debug)]
struct PreparedJobObject {
	job_port: JobPort,
	final_kill_on_drop: bool,
}

impl JobObject {
	fn prepare_job(
		&self,
		child: &mut dyn ChildWrapper,
		core: &CommandWrap,
	) -> Result<PreparedJobObject> {
		#[cfg(feature = "kill-on-drop")]
		let kill_on_drop = core.has_wrap::<KillOnDrop>();
		#[cfg(not(feature = "kill-on-drop"))]
		let kill_on_drop = false;

		let policy = job_creation_flags(user_creation_flags(core));

		#[cfg(feature = "tracing")]
		debug!(
			?kill_on_drop,
			resume_after_assignment = policy.resume_after_assignment,
			"options from other wrappers"
		);

		// Prefer the explicit capability, while preserving composition with transparent wrappers
		// written before `process_handle` was added.
		let handle = match child.try_process_handle() {
			Some(handle) => HANDLE(handle.as_raw_handle()),
			None => {
				terminate_child(child);
				return Err(Error::new(
					ErrorKind::Unsupported,
					"child wrapper does not expose a Windows process handle",
				));
			}
		};

		let job_port = match make_job_object(handle, true) {
			Ok(job_port) => job_port,
			Err(error) => {
				terminate_child(child);
				return Err(error);
			}
		};

		if policy.resume_after_assignment {
			let resumed = child
				.try_resume_after_job_assignment()
				.unwrap_or_else(|| resume_threads(handle));
			if let Err(error) = resumed {
				let _ = terminate_job(job_port.job, 1);
				terminate_child(child);
				return Err(error);
			}
		}

		Ok(PreparedJobObject {
			job_port,
			final_kill_on_drop: kill_on_drop,
		})
	}
}

impl CommandWrapper for JobObject {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_job_object();
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self, child)))]
	fn prepare_child(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		core: &CommandWrap,
	) -> Result<Option<Box<dyn Any + Send>>> {
		Ok(Some(Box::new(self.prepare_job(child, core)?)))
	}

	fn wrap_prepared_child(
		&mut self,
		inner: Box<dyn ChildWrapper>,
		prepared: Option<Box<dyn Any + Send>>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let prepared = prepared.expect("JobObject child preparation always produces state");
		let prepared = match prepared.downcast::<PreparedJobObject>() {
			Ok(prepared) => *prepared,
			Err(_) => unreachable!("JobObject prepared state retains its concrete type"),
		};
		Ok(Box::new(JobObjectChild::new(
			inner,
			prepared.job_port,
			prepared.final_kill_on_drop,
		)))
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self, inner)))]
	fn wrap_child(
		&mut self,
		mut inner: Box<dyn ChildWrapper>,
		core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let prepared = self.prepare_job(inner.as_mut(), core)?;
		let mut child = JobObjectChild::new(inner, prepared.job_port, prepared.final_kill_on_drop);
		child.disarm_job_object_layer()?;
		Ok(Box::new(child))
	}
}

/// Wrapper for `Child` which waits on all processes within the job.
#[derive(Debug)]
pub struct JobObjectChild {
	inner: Box<dyn ChildWrapper>,
	exit_status: ChildExitStatus,
	job_port: JobPort,
	final_kill_on_drop: bool,
	spawn_finalized: bool,
}

impl JobObjectChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(job_port)))]
	pub(crate) fn new(
		inner: Box<dyn ChildWrapper>,
		job_port: JobPort,
		final_kill_on_drop: bool,
	) -> Self {
		Self {
			inner,
			exit_status: ChildExitStatus::Running,
			job_port,
			final_kill_on_drop,
			spawn_finalized: false,
		}
	}
}

impl ChildWrapper for JobObjectChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		let Self {
			inner,
			job_port,
			final_kill_on_drop,
			spawn_finalized,
			..
		} = *self;
		if spawn_finalized && final_kill_on_drop {
			// manually drop the completion port
			let its = std::mem::ManuallyDrop::new(job_port);
			// SAFETY: `its` owns the completion-port handle and suppresses `JobPort::drop`.
			unsafe { CloseHandle(HANDLE(its.completion_port.as_raw_handle())) }.ok();
			// we leave the job handle unclosed, otherwise the Child is useless
			// (as closing it may terminate the job)
		}
		// Before spawn finalization, dropping the still-armed job instead guarantees that removing this
		// layer cannot let descendants escape a later lifecycle failure.

		inner
	}
	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		self.inner.try_process_handle()
	}
	fn owns_job_object_cleanup_layer(&self) -> bool {
		true
	}
	fn disarm_job_object_layer(&mut self) -> Result<()> {
		set_job_kill_on_drop(self.job_port.job, self.final_kill_on_drop)?;
		self.spawn_finalized = true;
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		terminate_job(self.job_port.job, 1)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
		Box::pin(async {
			if let ChildExitStatus::Exited(status) = &self.exit_status {
				return Ok(*status);
			}

			const MAX_RETRY_ATTEMPT: usize = 10;

			// always wait for parent to exit first, as by the time it does,
			// it's likely that all its children have already exited.
			let status = self.inner.wait().await?;
			self.exit_status = ChildExitStatus::Exited(status);

			// nevertheless, now try reaping all children a few times...
			for _ in 1..MAX_RETRY_ATTEMPT {
				if wait_on_job(
					self.job_port.completion_port.as_handle(),
					Some(Duration::ZERO),
				)?
				.is_break()
				{
					return Ok(status);
				}
			}

			// ...finally, if there are some that are still alive,
			// block in the background to reap them fully. The detached closure owns only a duplicate
			// completion-port handle so canceling this future cannot make its native wait stale or keep
			// the JobObject itself alive.
			let completion_port = self.job_port.completion_port.try_clone()?;
			let _ = spawn_blocking(move || blocking_wait_on_job(completion_port)).await??;
			Ok(status)
		})
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		let _ = wait_on_job(
			self.job_port.completion_port.as_handle(),
			Some(Duration::ZERO),
		)?;
		self.inner.try_wait()
	}
}

#[cfg(test)]
mod tests {
	use std::{
		future::Future,
		io,
		os::windows::{
			io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle},
			process::ExitStatusExt,
		},
		pin::Pin,
		process::ExitStatus,
		sync::mpsc,
		time::Duration,
	};

	use windows::Win32::{
		Foundation::{
			DUPLICATE_SAME_ACCESS, DuplicateHandle, GetHandleInformation, HANDLE,
			INVALID_HANDLE_VALUE,
		},
		System::{
			IO::{CreateIoCompletionPort, PostQueuedCompletionStatus},
			JobObjects::CreateJobObjectW,
			Threading::{CreateEventW, GetCurrentProcess},
		},
	};

	use super::*;
	use crate::windows::{JobHandle, JobPort};

	#[derive(Debug)]
	struct ImmediateChild;

	impl ChildWrapper for ImmediateChild {
		fn inner(&self) -> &dyn ChildWrapper {
			self
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self
		}

		fn id(&self) -> Option<u32> {
			Some(1)
		}

		fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
			Box::pin(async { Ok(ExitStatus::from_raw(0)) })
		}
	}

	struct ObserverGuard;

	impl ObserverGuard {
		fn install() -> (Self, mpsc::Receiver<BlockingWaitEvent>) {
			let (sender, receiver) = mpsc::channel();
			let mut observer = blocking_wait_observer().lock().unwrap();
			assert!(
				observer.is_none(),
				"blocking wait observer is already installed"
			);
			*observer = Some(sender);
			(Self, receiver)
		}
	}

	impl Drop for ObserverGuard {
		fn drop(&mut self) {
			*blocking_wait_observer().lock().unwrap() = None;
		}
	}

	fn owned(handle: HANDLE) -> OwnedHandle {
		// SAFETY: each caller transfers one newly created or duplicated owned handle.
		unsafe { OwnedHandle::from_raw_handle(handle.0) }
	}

	fn job_port() -> io::Result<JobPort> {
		// SAFETY: default attributes and no name create a uniquely owned job handle.
		let job = owned(unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?);
		// SAFETY: these arguments create a new completion port which is uniquely owned.
		let port = owned(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }?);
		Ok(JobPort {
			job: JobHandle(HANDLE(job.into_raw_handle())),
			completion_port: port,
		})
	}

	fn duplicate(handle: HANDLE) -> io::Result<OwnedHandle> {
		// SAFETY: this returns the current process pseudo-handle, which stays valid for this process.
		let current = unsafe { GetCurrentProcess() };
		let mut duplicate = HANDLE::default();
		// SAFETY: `handle` is live, `duplicate` is writable, and success transfers a distinct owned
		// handle in the current process.
		unsafe {
			DuplicateHandle(
				current,
				handle,
				current,
				&mut duplicate,
				0,
				false,
				DUPLICATE_SAME_ACCESS,
			)
		}?;
		Ok(owned(duplicate))
	}

	fn handle_is_live(raw: usize) -> bool {
		let mut flags = 0;
		// SAFETY: the call only queries the scalar handle value and writes `flags` on success.
		unsafe { GetHandleInformation(HANDLE(raw as *mut _), &mut flags) }.is_ok()
	}

	fn occupy_reused_handle(raw: usize) -> OwnedHandle {
		let mut retained = Vec::new();
		for _ in 0..4096 {
			// SAFETY: default security, manual reset, initially unsignaled, and no name create one event.
			let replacement = owned(unsafe { CreateEventW(None, true, false, None) }.unwrap());
			if replacement.as_raw_handle() as usize == raw {
				return replacement;
			}
			retained.push(replacement);
		}
		panic!("Windows did not reuse the closed completion-port slot under stress");
	}

	const CANCELED_WAIT_HELPER: &str = "PROCESS_WRAP_CANCELED_JOB_WAIT_HELPER";

	#[test]
	fn canceled_blocking_wait_owns_only_a_duplicate_completion_port() {
		if std::env::var_os(CANCELED_WAIT_HELPER).is_none() {
			let status = std::process::Command::new(std::env::current_exe().unwrap())
				.args([
					"tokio::job_object::tests::canceled_blocking_wait_owns_only_a_duplicate_completion_port",
					"--exact",
					"--nocapture",
				])
				.env(CANCELED_WAIT_HELPER, "1")
				.status()
				.unwrap();
			assert!(
				status.success(),
				"the isolated handle-reuse regression failed"
			);
			return;
		}

		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.max_blocking_threads(1)
			.enable_time()
			.build()
			.unwrap();
		runtime.block_on(async {
			let (blocker_started, blocker_started_rx) = mpsc::channel();
			let (release_blocker, release_blocker_rx) = mpsc::channel();
			let blocker = tokio::task::spawn_blocking(move || {
				blocker_started.send(()).unwrap();
				release_blocker_rx.recv().unwrap();
			});
			blocker_started_rx
				.recv_timeout(Duration::from_secs(2))
				.expect("the sole blocking worker starts");

			let job_port = job_port().unwrap();
			let original_job = job_port.job.0.0 as usize;
			let original = job_port.completion_port.as_raw_handle() as usize;
			let controller = duplicate(HANDLE(job_port.completion_port.as_raw_handle())).unwrap();
			let mut child = JobObjectChild::new(Box::new(ImmediateChild), job_port, false);
			let (_observer, observations) = ObserverGuard::install();

			let timed_out = tokio::time::timeout(Duration::from_millis(100), child.wait()).await;
			assert!(
				timed_out.is_err(),
				"the queued blocking wait must not start yet"
			);
			drop(child);
			assert!(
				!handle_is_live(original_job),
				"the delayed wait must not retain the JobObject"
			);
			assert!(
				!handle_is_live(original),
				"dropping the child closes the original port"
			);
			let replacement = occupy_reused_handle(original);

			release_blocker.send(()).unwrap();
			let begin = observations
				.recv_timeout(Duration::from_secs(2))
				.expect("the delayed completion-port wait begins");
			let BlockingWaitEvent::Begin { handle, owned } = begin else {
				panic!("the first observation must begin the wait: {begin:?}");
			};
			assert!(owned, "the delayed wait must retain an owned duplicate");
			assert_ne!(
				handle, original,
				"the delayed wait must not reuse the original slot"
			);
			assert!(
				handle_is_live(handle),
				"the delayed wait's duplicate is live"
			);
			// SAFETY: `controller` owns a live duplicate of the same completion port.
			unsafe { PostQueuedCompletionStatus(HANDLE(controller.as_raw_handle()), 0, 0, None) }
				.unwrap();
			assert_eq!(
				observations.recv_timeout(Duration::from_secs(2)).unwrap(),
				BlockingWaitEvent::End { handle }
			);
			assert_eq!(
				observations.recv_timeout(Duration::from_secs(2)).unwrap(),
				BlockingWaitEvent::Dropped { handle }
			);
			assert!(
				!handle_is_live(handle),
				"the duplicate closes after the wait returns"
			);
			assert!(
				handle_is_live(original),
				"the replacement object still owns the reused slot"
			);
			drop(replacement);
			blocker.await.unwrap();
		});
	}
}
