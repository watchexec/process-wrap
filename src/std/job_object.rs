use std::{
	any::Any,
	io::{Error, ErrorKind, Result},
	ops::ControlFlow,
	os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle},
	process::ExitStatus,
	time::{Duration, Instant},
};

#[cfg(feature = "tracing")]
use tracing::{debug, instrument};
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE},
	System::Threading::PROCESS_CREATION_FLAGS,
};

use crate::{
	ChildExitStatus,
	windows::{
		JOB_POLL_INTERVAL, JobPort, job_creation_flags, make_job_object, poll_job_drain,
		resume_threads, set_job_kill_on_drop, terminate_job,
	},
};

#[cfg(feature = "creation-flags")]
use super::CreationFlags;
use super::{
	ChildWrapper, ChildWrapperLayer, ChildWrapperSlots, CommandWrap, CommandWrapper,
	PendingChildWrapper, PreparedChild, PreparedChildRef, SpawnAttempt,
};

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
	if child.start_kill().is_ok() {
		let _ = child.wait();
	}
}

#[derive(Debug)]
struct PreparedJobObject {
	job_port: Option<JobPort>,
}

impl JobObject {
	fn prepare_job(
		&self,
		child: &mut dyn ChildWrapper,
		core: &CommandWrap,
	) -> Result<PreparedJobObject> {
		let policy = job_creation_flags(user_creation_flags(core));

		#[cfg(feature = "tracing")]
		debug!(
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
			job_port: Some(job_port),
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
		_inner: &mut dyn ChildWrapper,
		prepared: Option<PreparedChildRef<'_>>,
		_core: &CommandWrap,
	) -> Result<Option<PendingChildWrapper>> {
		let prepared = prepared.expect("JobObject child preparation always produces state");
		assert!(
			prepared.is::<PreparedJobObject>(),
			"JobObject prepared state retains its concrete type"
		);
		Ok(Some(PendingChildWrapper::new(JobObjectChild::detached(
			false,
		))))
	}
}

/// Wrapper for `Child` which waits on all processes within the job.
#[derive(Debug)]
pub struct JobObjectChild {
	inner: Option<Box<dyn ChildWrapper>>,
	prepared: Option<PreparedChild>,
	exit_status: ChildExitStatus,
	job_drained: bool,
	final_kill_on_drop: bool,
	spawn_finalized: bool,
}

impl JobObjectChild {
	pub(crate) fn detached(final_kill_on_drop: bool) -> Self {
		Self {
			inner: None,
			prepared: None,
			exit_status: ChildExitStatus::Running,
			job_drained: false,
			final_kill_on_drop,
			spawn_finalized: false,
		}
	}

	fn inner_ref(&self) -> &dyn ChildWrapper {
		self.inner
			.as_deref()
			.expect("an installed JobObject layer owns its child")
	}

	fn inner_mut_ref(&mut self) -> &mut dyn ChildWrapper {
		self.inner
			.as_deref_mut()
			.expect("an installed JobObject layer owns its child")
	}

	fn prepared(&self) -> &PreparedChild {
		self.prepared
			.as_ref()
			.expect("an installed JobObject layer owns its prepared state")
	}

	fn with_job_port<R>(&self, inspect: impl FnOnce(&JobPort) -> R) -> R {
		self.prepared()
			.with::<PreparedJobObject, _>(|prepared| {
				inspect(
					prepared
						.job_port
						.as_ref()
						.expect("the installed JobObject layer retains its job handles"),
				)
			})
			.expect("JobObject prepared state retains its concrete type")
	}
}

impl ChildWrapperLayer for JobObjectChild {
	fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
		ChildWrapperSlots::new(&mut self.inner)
			.with_prepared::<PreparedJobObject>(&mut self.prepared)
	}
}

fn wait_for_exit_and_job_drain_with(
	exit_status: &mut ChildExitStatus,
	job_drained: &mut bool,
	mut wait_direct: impl FnMut() -> Result<ExitStatus>,
	mut poll_drain: impl FnMut(Duration) -> Result<ControlFlow<()>>,
	mut now: impl FnMut() -> Instant,
	mut sleep: impl FnMut(Duration),
	mut on_pending: impl FnMut(ChildExitStatus),
) -> Result<ExitStatus> {
	let status = match *exit_status {
		ChildExitStatus::Running => {
			let status = wait_direct()?;
			*exit_status = ChildExitStatus::Exited(status);
			status
		}
		ChildExitStatus::Exited(status) => status,
	};

	while !*job_drained {
		let poll_started = now();
		if poll_drain(JOB_POLL_INTERVAL)?.is_break() {
			*job_drained = true;
		} else {
			let elapsed = now().saturating_duration_since(poll_started);
			let remaining = JOB_POLL_INTERVAL.saturating_sub(elapsed);
			on_pending(*exit_status);
			if !remaining.is_zero() {
				sleep(remaining);
			}
		}
	}
	Ok(status)
}

impl ChildWrapper for JobObjectChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner_ref()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner_mut_ref()
	}
	fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
		let inner = self
			.inner
			.take()
			.expect("an installed JobObject layer owns its child");
		if self.spawn_finalized && self.final_kill_on_drop {
			self.prepared()
				.with_mut::<PreparedJobObject, _>(|prepared| {
					let job_port = prepared
						.job_port
						.take()
						.expect("the installed JobObject layer retains its job handles");
					// Manually close the completion port while retaining the job handle. Closing a
					// kill-on-close job here would make the extracted child unusable.
					let job_port = std::mem::ManuallyDrop::new(job_port);
					// SAFETY: `job_port` owns the completion-port handle and suppresses `JobPort::drop`.
					unsafe { CloseHandle(HANDLE(job_port.completion_port.as_raw_handle())) }.ok();
				})
				.expect("JobObject prepared state retains its concrete type");
		}
		// Before spawn finalization, dropping the still-armed prepared job instead guarantees that
		// removing this layer cannot let descendants escape a later lifecycle failure.

		inner
	}
	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		self.inner_ref().try_process_handle()
	}
	fn owns_job_object_cleanup_layer(&self) -> bool {
		true
	}
	fn disarm_job_object_layer(&mut self) -> Result<()> {
		#[cfg(test)]
		crate::windows::test_support::fail_final_owner()?;
		let final_kill_on_drop = self.final_kill_on_drop;
		self.with_job_port(|job_port| set_job_kill_on_drop(job_port.job, final_kill_on_drop))?;
		self.spawn_finalized = true;
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		self.with_job_port(|job_port| terminate_job(job_port.job, 1))
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Result<ExitStatus> {
		let Self {
			inner,
			prepared,
			exit_status,
			job_drained,
			..
		} = self;
		let prepared = prepared
			.as_ref()
			.expect("an installed JobObject layer owns its prepared state");
		let inner = inner
			.as_deref_mut()
			.expect("an installed JobObject layer owns its child");
		wait_for_exit_and_job_drain_with(
			exit_status,
			job_drained,
			|| inner.wait(),
			|timeout| {
				prepared
					.with::<PreparedJobObject, _>(|prepared| {
						let job_port = prepared
							.job_port
							.as_ref()
							.expect("the installed JobObject layer retains its job handles");
						poll_job_drain(job_port.job, job_port.completion_port.as_handle(), timeout)
					})
					.expect("JobObject prepared state retains its concrete type")
			},
			Instant::now,
			std::thread::sleep,
			|_| {},
		)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		if matches!(self.exit_status, ChildExitStatus::Running) {
			let Some(status) = self.inner_mut_ref().try_wait()? else {
				return Ok(None);
			};
			self.exit_status = ChildExitStatus::Exited(status);
		}

		if !self.job_drained
			&& self
				.with_job_port(|job_port| {
					poll_job_drain(
						job_port.job,
						job_port.completion_port.as_handle(),
						Duration::ZERO,
					)
				})?
				.is_break()
		{
			self.job_drained = true;
		}

		match (self.exit_status, self.job_drained) {
			(ChildExitStatus::Exited(status), true) => Ok(Some(status)),
			_ => Ok(None),
		}
	}
}

#[cfg(test)]
mod tests {
	use std::{
		cell::{Cell, RefCell},
		os::windows::{io::AsHandle, process::CommandExt, process::ExitStatusExt},
		panic::{AssertUnwindSafe, catch_unwind},
		sync::{Arc, atomic::Ordering, mpsc},
		thread,
	};

	use windows::Win32::System::Threading::CREATE_SUSPENDED;

	use crate::std::{ProviderProduct, SpawnProvider};
	use crate::windows::test_support::{
		LifecycleState, OwnerError, OwnerFailure, OwnerPanic, PanickingCommittedTransaction,
		ProcessGuard, TreePaths, arm_extra_prepared_owner, arm_owner_failure,
		assert_tree_terminated, clear_extra_prepared_owners, clear_owner_failure,
		observe_descendant, publish_process_guards,
	};

	use super::*;

	#[derive(Debug)]
	struct TreeProvider {
		paths: TreePaths,
		state: Arc<LifecycleState>,
	}

	impl SpawnProvider for TreeProvider {
		fn spawn(
			&self,
			attempt: &mut SpawnAttempt,
			_command: &CommandWrap,
		) -> Result<ProviderProduct> {
			let policy = attempt.windows_spawn_policy();
			assert!(policy.has_job_object());
			assert!(policy.is_temporarily_suspended());
			let mut command = self.paths.direct_command()?;
			command.creation_flags(CREATE_SUSPENDED.0);
			let child = command.spawn()?;
			let rollback_guard = publish_process_guards(&self.state, child.as_handle())?;
			Ok(ProviderProduct::new(
				Box::new(child),
				Box::new(PanickingCommittedTransaction::new(
					Arc::clone(&self.state),
					rollback_guard,
				)),
			))
		}
	}

	#[derive(Debug)]
	struct TreeProviderWrapper(TreeProvider);

	impl CommandWrapper for TreeProviderWrapper {
		fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
			Some(&self.0)
		}
	}

	#[derive(Debug)]
	struct ObserveTree {
		paths: TreePaths,
		state: Arc<LifecycleState>,
	}

	impl CommandWrapper for ObserveTree {
		fn post_spawn(
			&mut self,
			_attempt: &mut SpawnAttempt,
			_child: &mut dyn ChildWrapper,
			_command: &CommandWrap,
		) -> Result<()> {
			observe_descendant(&self.paths, &self.state)
		}
	}

	#[derive(Debug)]
	struct ObserveNativeTree(ObserveTree);

	impl CommandWrapper for ObserveNativeTree {
		fn post_spawn(
			&mut self,
			_attempt: &mut SpawnAttempt,
			child: &mut dyn ChildWrapper,
			_command: &CommandWrap,
		) -> Result<()> {
			let handle = child
				.try_process_handle()
				.ok_or_else(|| Error::other("the native child has no process handle"))?;
			let guard = ProcessGuard::clone_from(handle)?;
			*self
				.0
				.state
				.direct
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(guard);
			observe_descendant(&self.0.paths, &self.0.state)
		}
	}

	fn primary_owner_failure_preserved(
		outcome: std::thread::Result<Result<Box<dyn ChildWrapper>>>,
		identity: &Arc<()>,
		was_panic: bool,
	) -> bool {
		if was_panic {
			let payload = match outcome {
				Err(payload) => payload,
				Ok(_) => return false,
			};
			match payload.downcast::<OwnerPanic>() {
				Ok(payload) => Arc::ptr_eq(&payload.0, identity),
				Err(secondary) => {
					std::mem::forget(secondary);
					false
				}
			}
		} else {
			let result = match outcome {
				Ok(result) => result,
				Err(secondary) => {
					std::mem::forget(secondary);
					return false;
				}
			};
			let error = match result {
				Err(error) => error,
				Ok(_) => return false,
			};
			error
				.get_ref()
				.and_then(|error| error.downcast_ref::<OwnerError>())
				.is_some_and(|error| Arc::ptr_eq(&error.0, identity))
		}
	}

	fn topology_command(
		provider: bool,
		paths: &TreePaths,
		state: &Arc<LifecycleState>,
	) -> Result<CommandWrap> {
		let mut command = if provider {
			CommandWrap::new("provider-owned-program")
		} else {
			CommandWrap::from(paths.direct_command()?)
		};
		command.wrap(JobObject);
		let observer = ObserveTree {
			paths: paths.clone(),
			state: Arc::clone(state),
		};
		if provider {
			command
				.wrap(observer)
				.wrap(TreeProviderWrapper(TreeProvider {
					paths: paths.clone(),
					state: Arc::clone(state),
				}));
		} else {
			command.wrap(ObserveNativeTree(observer));
		}
		Ok(command)
	}

	#[test]
	fn unexpected_prepared_owner_fails_native_and_provider_spawns_before_success() -> Result<()> {
		for provider in [false, true] {
			let directory = tempfile::tempdir()?;
			let paths = TreePaths::new(directory.path());
			let state = Arc::new(LifecycleState::default());
			let mut command = topology_command(provider, &paths, &state)?;
			arm_extra_prepared_owner();

			let error = command
				.spawn()
				.expect_err("an unexpected prepared owner must fail the lifecycle");
			let (injection_pending, retained_owners) = clear_extra_prepared_owners();
			assert!(
				!injection_pending,
				"the installation must consume the injection"
			);
			assert_eq!(retained_owners, 1);
			assert_eq!(error.kind(), ErrorKind::InvalidInput);
			assert_eq!(
				error.to_string(),
				"prepared child state has an unexpected custody topology"
			);
			assert_eq!(state.commits.load(Ordering::SeqCst), 0);
			assert_eq!(
				state.rollbacks.load(Ordering::SeqCst),
				usize::from(provider)
			);
			assert_tree_terminated(&paths, &state)?;
			std::fs::remove_file(&paths.descendant_pid)?;

			let mut child = command.spawn().expect("the command remains reusable");
			child.start_kill()?;
			let _ = child.wait()?;
			let disposal = catch_unwind(AssertUnwindSafe(|| drop(child)));
			if provider {
				let payload =
					disposal.expect_err("the provider fixture residue panics on disposal");
				std::mem::forget(payload);
			} else {
				disposal.expect("native child disposal does not panic");
			}
			assert_tree_terminated(&paths, &state)?;
			assert_eq!(state.commits.load(Ordering::SeqCst), usize::from(provider));
			assert_eq!(
				state.rollbacks.load(Ordering::SeqCst),
				usize::from(provider)
			);
		}
		Ok(())
	}

	#[test]
	fn final_owner_failures_terminate_the_real_job_tree_and_preserve_identity() -> Result<()> {
		for was_panic in [false, true] {
			let directory = tempfile::tempdir()?;
			let paths = TreePaths::new(directory.path());
			let state = Arc::new(LifecycleState::default());
			let identity = Arc::new(());
			let failure = if was_panic {
				OwnerFailure::Panic(Arc::clone(&identity))
			} else {
				OwnerFailure::Error(Arc::clone(&identity))
			};
			arm_owner_failure(failure);

			let mut command = CommandWrap::new("provider-owned-program");
			command
				.wrap(JobObject)
				.wrap(ObserveTree {
					paths: paths.clone(),
					state: Arc::clone(&state),
				})
				.wrap(TreeProviderWrapper(TreeProvider {
					paths: paths.clone(),
					state: Arc::clone(&state),
				}));
			let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
			let failure_was_not_consumed = clear_owner_failure();
			let primary_preserved = primary_owner_failure_preserved(outcome, &identity, was_panic);
			let tree_result = assert_tree_terminated(&paths, &state);

			assert!(
				!failure_was_not_consumed,
				"the final owner hook did not run"
			);
			assert!(
				primary_preserved,
				"cleanup replaced the final owner failure"
			);
			assert_eq!(state.commits.load(Ordering::SeqCst), 1);
			assert_eq!(state.rollbacks.load(Ordering::SeqCst), 0);
			assert_eq!(state.residue_drops.load(Ordering::SeqCst), 1);
			assert_eq!(state.payload_drops.load(Ordering::SeqCst), 0);
			tree_result?;
		}
		Ok(())
	}

	#[test]
	fn sustained_nonterminal_wakes_preserve_poll_cadence() -> Result<()> {
		let status = ExitStatus::from_raw(0);
		let mut exit_status = ChildExitStatus::Exited(status);
		let mut job_drained = false;
		let base = Instant::now();
		let elapsed = Cell::new(Duration::ZERO);
		let poll_costs = [
			Duration::from_millis(2),
			Duration::from_millis(7),
			JOB_POLL_INTERVAL,
			Duration::ZERO,
		];
		let mut poll_costs = poll_costs.into_iter();
		let mut outcomes = [
			ControlFlow::Continue(()),
			ControlFlow::Continue(()),
			ControlFlow::Continue(()),
			ControlFlow::Break(()),
		]
		.into_iter();
		let poll_starts = RefCell::new(Vec::new());
		let sleeps = RefCell::new(Vec::new());

		let result = wait_for_exit_and_job_drain_with(
			&mut exit_status,
			&mut job_drained,
			|| unreachable!("the direct status was already cached"),
			|timeout| {
				assert_eq!(timeout, JOB_POLL_INTERVAL);
				poll_starts.borrow_mut().push(elapsed.get());
				elapsed.set(elapsed.get() + poll_costs.next().expect("one cost per injected poll"));
				Ok(outcomes.next().expect("one outcome per injected poll"))
			},
			|| base + elapsed.get(),
			|duration| {
				sleeps.borrow_mut().push(duration);
				elapsed.set(elapsed.get() + duration);
			},
			|_| {},
		)?;

		assert_eq!(result, status);
		assert!(job_drained);
		assert_eq!(
			poll_starts.into_inner(),
			[
				Duration::ZERO,
				JOB_POLL_INTERVAL,
				JOB_POLL_INTERVAL * 2,
				JOB_POLL_INTERVAL * 3,
			]
		);
		assert_eq!(
			sleeps.into_inner(),
			[Duration::from_millis(8), Duration::from_millis(3)]
		);
		Ok(())
	}

	#[test]
	fn blocking_wait_rendezvous_follows_cached_exit_and_false_drain_poll() -> Result<()> {
		let (pending_tx, pending_rx) = mpsc::sync_channel(0);
		let (release_tx, release_rx) = mpsc::channel();
		let (completed_tx, completed_rx) = mpsc::channel();
		let waiter = thread::spawn(move || {
			let status = ExitStatus::from_raw(0);
			let mut exit_status = ChildExitStatus::Running;
			let mut job_drained = false;
			let mut polls = [ControlFlow::Continue(()), ControlFlow::Break(())].into_iter();
			let result = wait_for_exit_and_job_drain_with(
				&mut exit_status,
				&mut job_drained,
				|| Ok(status),
				|_| {
					Ok(polls
						.next()
						.expect("the fake job drains on its second poll"))
				},
				Instant::now,
				|_| {},
				|cached_exit| {
					assert!(matches!(
						cached_exit,
						ChildExitStatus::Exited(cached) if cached == status
					));
					pending_tx
						.send(())
						.expect("the test receives the pending rendezvous");
					release_rx
						.recv()
						.expect("the test releases the pending drain");
				},
			);
			let _ = completed_tx.send((result, exit_status, job_drained));
		});

		if let Err(error) = pending_rx.recv_timeout(Duration::from_secs(5)) {
			let _ = release_tx.send(());
			let _ = waiter.join();
			return Err(Error::new(
				ErrorKind::TimedOut,
				format!("wait did not enter the pending drain state: {error}"),
			));
		}
		let was_pending = matches!(completed_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
		let _ = release_tx.send(());
		let completed = completed_rx.recv_timeout(Duration::from_secs(5));
		let joined = waiter.join();

		assert!(
			was_pending,
			"wait returned before the injected drain release"
		);
		joined.map_err(|_| Error::other("blocking wait seam thread panicked"))?;
		let (result, exit_status, job_drained) = completed.map_err(|error| {
			Error::new(
				ErrorKind::TimedOut,
				format!("wait did not finish after the injected release: {error}"),
			)
		})?;
		let status = result?;
		assert!(status.success());
		assert!(matches!(
			exit_status,
			ChildExitStatus::Exited(cached) if cached == status
		));
		assert!(job_drained);
		Ok(())
	}
}
