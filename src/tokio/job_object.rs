use std::{
	any::Any,
	future::Future,
	io::{Error, ErrorKind, Result},
	os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle},
	pin::Pin,
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
#[cfg(feature = "kill-on-drop")]
use super::KillOnDrop;
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
	let _ = child.start_kill();
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
		#[cfg(feature = "kill-on-drop")]
		let final_kill_on_drop = _core.has_wrap::<KillOnDrop>();
		#[cfg(not(feature = "kill-on-drop"))]
		let final_kill_on_drop = false;
		Ok(Some(PendingChildWrapper::new(JobObjectChild::detached(
			final_kill_on_drop,
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
	poll_cadence: Option<tokio::task::JoinHandle<()>>,
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
			poll_cadence: None,
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

	fn arm_poll_cadence(&mut self, elapsed: Duration) {
		debug_assert!(self.poll_cadence.is_none());
		let remaining = JOB_POLL_INTERVAL.saturating_sub(elapsed);
		if !remaining.is_zero() {
			// The blocking task owns only inert timing data. In particular, it cannot prolong or
			// outlive any borrow of the JobObject or completion-port handles.
			self.poll_cadence = Some(tokio::task::spawn_blocking(move || {
				std::thread::sleep(remaining);
			}));
		}
	}

	async fn wait_for_poll_cadence(&mut self) -> Result<()> {
		let Some(cadence) = self.poll_cadence.as_mut() else {
			return Ok(());
		};
		let result = cadence.await;
		self.poll_cadence = None;
		result.map_err(Error::other)
	}

	fn clear_poll_cadence(&mut self) {
		if let Some(cadence) = self.poll_cadence.take() {
			// `abort` can prevent a queued blocking task from starting. If it has started, Tokio
			// lets the short sleep finish detached; its closure owns no native resource.
			cadence.abort();
		}
	}
}

impl ChildWrapperLayer for JobObjectChild {
	fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
		ChildWrapperSlots::new(&mut self.inner)
			.with_prepared::<PreparedJobObject>(&mut self.prepared)
	}
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
		#[cfg(test)]
		crate::windows::test_support::record_owner_event("owner-disarmed");
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		self.with_job_port(|job_port| terminate_job(job_port.job, 1))
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
		Box::pin(async {
			let status = match self.exit_status {
				ChildExitStatus::Running => {
					// Cache direct-child exit independently from whole-job drain. There is no await
					// between observing the status and retaining it, so cancellation cannot lose it.
					let status = self.inner_mut_ref().wait().await?;
					self.exit_status = ChildExitStatus::Exited(status);
					status
				}
				ChildExitStatus::Exited(status) => status,
			};

			while !self.job_drained {
				// A canceled wait leaves this handle in `self`, so its successor cannot poll again
				// until the already-armed cadence completes.
				self.wait_for_poll_cadence().await?;
				let poll_started = Instant::now();
				if self
					.with_job_port(|job_port| {
						poll_job_drain(
							job_port.job,
							job_port.completion_port.as_handle(),
							Duration::ZERO,
						)
					})?
					.is_break()
				{
					// No await occurs between the authoritative accounting result and this durable
					// transition, so a canceled future cannot consume the drain state.
					self.job_drained = true;
					self.clear_poll_cadence();
					break;
				}
				self.arm_poll_cadence(poll_started.elapsed());
			}
			Ok(status)
		})
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
			self.clear_poll_cadence();
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
		os::windows::{io::BorrowedHandle, process::CommandExt},
		panic::{AssertUnwindSafe, catch_unwind, panic_any},
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
	};

	use windows::Win32::System::Threading::CREATE_SUSPENDED;

	use crate::tokio::{ProviderProduct, SpawnProvider};
	use crate::windows::test_support::{
		LifecycleState, OwnerError, OwnerFailure, OwnerPanic, PanickingCommittedTransaction,
		ProcessGuard, TreePaths, arm_extra_prepared_owner, arm_owner_events, arm_owner_failure,
		assert_tree_terminated, clear_extra_prepared_owners, clear_owner_events,
		clear_owner_failure, observe_descendant, publish_process_guards, record_owner_event,
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
			let mut command = tokio::process::Command::from(command);
			let child = command.spawn()?;
			let handle = child
				.raw_handle()
				.ok_or_else(|| Error::other("the provider child has no process handle"))?;
			// SAFETY: `child` owns this process handle throughout both cloning operations.
			let handle = unsafe { BorrowedHandle::borrow_raw(handle) };
			let rollback_guard = publish_process_guards(&self.state, handle)?;
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

	#[derive(Debug)]
	struct PostOwnerAccessorLayer {
		inner: Option<Box<dyn ChildWrapper>>,
		prepared: Option<PreparedChild>,
		slot_calls: Arc<AtomicUsize>,
		panic_on: usize,
	}

	impl ChildWrapperLayer for PostOwnerAccessorLayer {
		fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
			let call = self.slot_calls.fetch_add(1, Ordering::SeqCst) + 1;
			record_owner_event("slot-accessor");
			if call == self.panic_on {
				panic_any("slot accessor ran after final owner disarm");
			}
			ChildWrapperSlots::new(&mut self.inner).with_prepared::<()>(&mut self.prepared)
		}
	}

	impl ChildWrapper for PostOwnerAccessorLayer {
		fn inner(&self) -> &dyn ChildWrapper {
			self.inner
				.as_deref()
				.expect("an installed accessor layer owns its child")
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.inner
				.as_deref_mut()
				.expect("an installed accessor layer owns its child")
		}

		fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.inner
				.take()
				.expect("an installed accessor layer owns its child")
		}
	}

	#[derive(Debug)]
	struct PostOwnerAccessor {
		slot_calls: Arc<AtomicUsize>,
		panic_on: usize,
	}

	impl CommandWrapper for PostOwnerAccessor {
		fn prepare_child(
			&mut self,
			_attempt: &mut SpawnAttempt,
			_child: &mut dyn ChildWrapper,
			_command: &CommandWrap,
		) -> Result<Option<Box<dyn Any + Send>>> {
			self.slot_calls.store(0, Ordering::SeqCst);
			Ok(Some(Box::new(())))
		}

		fn wrap_prepared_child(
			&mut self,
			_child: &mut dyn ChildWrapper,
			prepared: Option<PreparedChildRef<'_>>,
			_command: &CommandWrap,
		) -> Result<Option<PendingChildWrapper>> {
			assert!(prepared.as_ref().is_some_and(PreparedChildRef::is::<()>));
			Ok(Some(PendingChildWrapper::new(PostOwnerAccessorLayer {
				inner: None,
				prepared: None,
				slot_calls: Arc::clone(&self.slot_calls),
				panic_on: self.panic_on,
			})))
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
			CommandWrap::from(tokio::process::Command::from(paths.direct_command()?))
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

	#[tokio::test(flavor = "current_thread")]
	async fn unexpected_prepared_owner_fails_native_and_provider_spawns_before_success()
	-> Result<()> {
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
			let _ = child.wait().await?;
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

	#[tokio::test(flavor = "current_thread")]
	async fn no_slot_accessor_runs_after_final_owner_disarm() -> Result<()> {
		for provider in [false, true] {
			let directory = tempfile::tempdir()?;
			let paths = TreePaths::new(directory.path());
			let state = Arc::new(LifecycleState::default());
			let slot_calls = Arc::new(AtomicUsize::new(0));
			let events = Arc::new(std::sync::Mutex::new(Vec::new()));
			let mut command = topology_command(provider, &paths, &state)?;
			command.wrap(PostOwnerAccessor {
				slot_calls: Arc::clone(&slot_calls),
				panic_on: if provider { 4 } else { 3 },
			});

			for attempt in 0..2 {
				events
					.lock()
					.unwrap_or_else(std::sync::PoisonError::into_inner)
					.clear();
				arm_owner_events(Arc::clone(&events));
				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				let mut succeeded = false;
				match outcome {
					Ok(Ok(mut child)) => {
						child.start_kill()?;
						let _ = child.wait().await?;
						let disposal = catch_unwind(AssertUnwindSafe(|| drop(child)));
						if provider {
							let payload = disposal
								.expect_err("the provider fixture residue panics on disposal");
							std::mem::forget(payload);
						} else {
							disposal.expect("native child disposal does not panic");
						}
						succeeded = true;
					}
					Ok(Err(_)) => {}
					Err(payload) => std::mem::forget(payload),
				}
				assert!(
					clear_owner_events(),
					"the owner event barrier remained armed"
				);
				let tree_result = assert_tree_terminated(&paths, &state);
				assert!(
					succeeded,
					"a post-owner-only accessor panic must remain unreachable"
				);
				assert_eq!(slot_calls.load(Ordering::SeqCst), 1);
				assert_eq!(
					*events
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner),
					["slot-accessor", "owner-disarmed"],
					"the final owner transition follows the last caller-defined accessor"
				);
				tree_result?;
				if attempt == 0 {
					std::fs::remove_file(&paths.descendant_pid)?;
				}
			}
		}
		Ok(())
	}

	#[tokio::test(flavor = "current_thread")]
	async fn final_owner_failures_terminate_the_real_job_tree_and_preserve_identity() -> Result<()>
	{
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
}
