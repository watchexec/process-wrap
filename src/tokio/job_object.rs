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
use windows::Win32::{Foundation::HANDLE, System::Threading::PROCESS_CREATION_FLAGS};

use crate::{
	ChildExitStatus,
	windows::{
		JOB_POLL_INTERVAL, JobPort, job_creation_flags, make_job_object, poll_job_drain,
		resume_threads, terminate_job,
	},
};

#[cfg(feature = "kill-on-drop")]
use crate::windows::release_extracted_job_port;

#[cfg(not(test))]
use crate::windows::set_job_kill_on_drop;
#[cfg(test)]
use crate::windows::set_job_kill_on_drop_observed;

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
/// `KillOnDrop` may likewise be registered before or after `JobObject`. With both wrappers installed,
/// successful spawn enables `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so ordinary drop while the
/// `JobObject` layer remains installed terminates every process still associated with the job. Without
/// `JobObject`, the lower Tokio child's kill-on-drop policy targets only that direct child.
///
/// Consuming the `JobObject` layer through [`ChildWrapper::into_inner`] relinquishes whole-job waiting,
/// explicit whole-job killing, and kill-on-close supervision. The returned lower child remains usable
/// and retains only its direct-child kill-on-drop policy.
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
///
/// When `KillOnDrop` is also registered, ordinary drop while this layer remains installed closes the
/// final job handle and terminates every process still associated with the job. Consuming this layer
/// relinquishes whole-job wait, kill, and kill-on-close supervision; the returned lower child retains
/// only direct-child policy.
#[derive(Debug)]
pub struct JobObjectChild {
	inner: Option<Box<dyn ChildWrapper>>,
	prepared: Option<PreparedChild>,
	extracted: Option<PreparedJobObject>,
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
			extracted: None,
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
		if let Some(prepared) = self.extracted.as_ref() {
			return inspect(
				prepared
					.job_port
					.as_ref()
					.expect("the extracted JobObject layer retains its job handles"),
			);
		}
		self.prepared().with_required::<PreparedJobObject, _>(
			|| panic!("JobObject prepared state retains its concrete type"),
			|prepared| {
				let job_port = prepared
					.job_port
					.as_ref()
					.expect("the installed JobObject layer retains its job handles");
				inspect(job_port)
			},
		)
	}

	#[cfg(feature = "kill-on-drop")]
	fn take_prepared(&mut self) -> PreparedJobObject {
		if let Some(prepared) = self.extracted.take() {
			return prepared;
		}
		self.prepared()
			.take_prepared_value::<PreparedJobObject>()
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
		#[cfg(feature = "kill-on-drop")]
		if self.spawn_finalized && self.final_kill_on_drop {
			let job_drained = self.job_drained;
			let mut prepared = self.take_prepared();
			let job_port = prepared
				.job_port
				.take()
				.expect("the installed JobObject layer retains its job handles");
			release_extracted_job_port(job_port, job_drained);
		}
		// Before spawn finalization, dropping the still-armed prepared job instead guarantees that
		// removing this layer cannot let descendants escape a later lifecycle failure.

		inner
	}
	fn retain_prepared_after_sidecar_removal_layer(&mut self) {
		if self.extracted.is_none() {
			let prepared = self
				.prepared()
				.take_prepared_value::<PreparedJobObject>()
				.expect("JobObject prepared state retains its concrete type");
			self.extracted = Some(prepared);
		}
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
		#[cfg(test)]
		let transition_probe = crate::windows::test_support::owner_transition_probe();
		#[cfg(test)]
		self.with_job_port(|job_port| {
			crate::windows::test_support::record_owner_event("before-owner-transition");
			set_job_kill_on_drop_observed(job_port.job, final_kill_on_drop, transition_probe)
		})?;
		#[cfg(not(test))]
		self.with_job_port(|job_port| set_job_kill_on_drop(job_port.job, final_kill_on_drop))?;
		self.spawn_finalized = true;
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
	use crate::windows::{
		reset_job_port_custodian_for_test,
		test_support::{
			LifecycleState, OwnerError, OwnerFailure, OwnerPanic, PanickingCommittedTransaction,
			ProcessGuard, TreePaths, arm_extra_prepared_owner, arm_job_port_close_probe,
			arm_owner_events, arm_owner_failure, arm_owner_transition_probe,
			arm_spawn_cleanup_handle_probe, assert_tree_terminated, clear_extra_prepared_owners,
			clear_job_port_close_probe, clear_owner_events, clear_owner_failure,
			finish_owner_transition_probe, finish_spawn_cleanup_handle_probe,
			job_custodian_worker_starts, job_port_last_resort_retentions, observe_descendant,
			publish_process_guards, record_owner_event, reset_job_extraction_faults,
			serial_job_extraction, set_job_custodian_start_failures,
			set_job_extraction_disarm_failures, set_job_extraction_query_failures,
			spawn_cleanup_handle_close_count,
		},
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

	struct ExtractionFaultCleanup;

	impl Drop for ExtractionFaultCleanup {
		fn drop(&mut self) {
			reset_job_extraction_faults();
		}
	}

	fn extraction_command(exit_immediately: bool, job_first: bool) -> CommandWrap {
		let mut command = if exit_immediately {
			CommandWrap::with_new("cmd.exe", |command| {
				command.args(["/D", "/S", "/C", "exit /b 0"]);
			})
		} else {
			CommandWrap::with_new("cmd.exe", |command| {
				command.args(["/D", "/S", "/C", "ping -n 30 127.0.0.1 >NUL"]);
			})
		};
		if job_first {
			command.wrap(JobObject).wrap(KillOnDrop);
		} else {
			command.wrap(KillOnDrop).wrap(JobObject);
		}
		command
	}

	fn wait_for_job_port_closes(
		probe: &crate::windows::test_support::JobPortCloseProbe,
		expected: (usize, usize),
	) {
		let deadline = Instant::now() + Duration::from_secs(5);
		while probe.counts() != expected {
			assert!(
				Instant::now() < deadline,
				"JobObject custodian did not close both handles: {:?}",
				probe.counts()
			);
			std::thread::sleep(Duration::from_millis(10));
		}
	}

	#[tokio::test(flavor = "current_thread")]
	async fn drained_kill_on_drop_extraction_closes_complete_job_ports_repeatedly() -> Result<()> {
		let _serial = serial_job_extraction();
		for job_first in [false, true] {
			for _ in 0..8 {
				let probe = arm_job_port_close_probe();
				let mut command = extraction_command(true, job_first);
				let mut child = command.spawn()?;
				clear_job_port_close_probe();
				let status = child.wait().await?;
				let mut lower = child.into_inner();
				assert_eq!(probe.counts(), (1, 1));
				assert_eq!(lower.wait().await?, status);
				assert_eq!(lower.try_wait()?, Some(status));
			}
		}
		Ok(())
	}

	#[tokio::test(flavor = "current_thread")]
	async fn live_kill_on_drop_extraction_disarms_and_closes_before_returning() -> Result<()> {
		let _serial = serial_job_extraction();
		for job_first in [false, true] {
			let probe = arm_job_port_close_probe();
			let mut command = extraction_command(false, job_first);
			let child = command.spawn()?;
			clear_job_port_close_probe();
			let mut lower = child.into_inner();
			assert_eq!(probe.counts(), (1, 1));
			assert_eq!(lower.try_wait()?, None);
			lower.start_kill()?;
			let status = lower.wait().await?;
			assert_eq!(lower.wait().await?, status);
		}
		Ok(())
	}

	#[tokio::test(flavor = "current_thread")]
	async fn extraction_failures_use_one_shared_custodian_then_close_every_port() -> Result<()> {
		let _serial = serial_job_extraction();
		let _fault_cleanup = ExtractionFaultCleanup;
		let worker_starts = job_custodian_worker_starts();
		set_job_extraction_query_failures(usize::MAX);
		set_job_extraction_disarm_failures(usize::MAX);
		let mut lowers = Vec::new();
		let mut probes = Vec::new();
		for _ in 0..2 {
			let probe = arm_job_port_close_probe();
			let mut command = extraction_command(false, true);
			let child = command.spawn()?;
			clear_job_port_close_probe();
			let mut lower = child.into_inner();
			assert_eq!(probe.counts(), (0, 0));
			assert_eq!(lower.try_wait()?, None);
			lowers.push(lower);
			probes.push(probe);
		}
		let deadline = Instant::now() + Duration::from_secs(5);
		while job_custodian_worker_starts() == worker_starts {
			assert!(
				Instant::now() < deadline,
				"the shared custodian did not start"
			);
			std::thread::sleep(Duration::from_millis(10));
		}
		assert_eq!(job_custodian_worker_starts(), worker_starts + 1);

		reset_job_extraction_faults();
		for lower in &mut lowers {
			lower.start_kill()?;
			let _ = lower.wait().await?;
		}
		for probe in &probes {
			wait_for_job_port_closes(probe, (1, 1));
		}
		assert_eq!(job_custodian_worker_starts(), worker_starts + 1);
		Ok(())
	}

	#[tokio::test(flavor = "current_thread")]
	async fn custodian_start_failure_uses_only_exact_last_resort_retention() -> Result<()> {
		let _serial = serial_job_extraction();
		let _fault_cleanup = ExtractionFaultCleanup;
		reset_job_port_custodian_for_test();
		let retained = job_port_last_resort_retentions();
		set_job_extraction_query_failures(1);
		set_job_extraction_disarm_failures(1);
		set_job_custodian_start_failures(1);
		let probe = arm_job_port_close_probe();
		let mut command = extraction_command(false, true);
		let child = command.spawn()?;
		clear_job_port_close_probe();
		let mut lower = child.into_inner();

		assert_eq!(probe.counts(), (0, 0));
		assert_eq!(job_port_last_resort_retentions(), retained + 1);
		assert_eq!(lower.try_wait()?, None);
		lower.start_kill()?;
		let status = lower.wait().await?;
		assert_eq!(lower.wait().await?, status);
		assert_eq!(probe.counts(), (0, 0));
		Ok(())
	}

	#[tokio::test(flavor = "current_thread")]
	async fn native_spawn_cleanup_handle_closes_with_returned_child() -> Result<()> {
		let directory = tempfile::tempdir()?;
		let paths = TreePaths::new(directory.path());
		let state = Arc::new(LifecycleState::default());
		let mut command = topology_command(false, &paths, &state)?;

		for attempt in 0..2 {
			arm_spawn_cleanup_handle_probe();
			let mut child = command.spawn()?;
			assert_eq!(
				spawn_cleanup_handle_close_count(),
				0,
				"the exact duplicate process handle remains owned through public spawn return"
			);
			child.start_kill()?;
			let _ = child.wait().await?;
			assert_eq!(
				spawn_cleanup_handle_close_count(),
				0,
				"child operations retain cleanup-handle custody"
			);
			if attempt == 0 {
				drop(child);
			} else {
				let child = child.into_inner();
				assert_eq!(
					spawn_cleanup_handle_close_count(),
					1,
					"consuming the custody sidecar closes the exact duplicate once"
				);
				drop(child);
			}
			assert_eq!(
				spawn_cleanup_handle_close_count(),
				1,
				"returned-child disposal closes the exact duplicate process handle once"
			);
			assert_eq!(finish_spawn_cleanup_handle_probe(), 1);
			assert_tree_terminated(&paths, &state)?;
			if attempt == 0 {
				std::fs::remove_file(&paths.descendant_pid)?;
			}
		}
		Ok(())
	}

	#[tokio::test(flavor = "current_thread")]
	async fn native_final_owner_failures_close_armed_spawn_cleanup_once() -> Result<()> {
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
			let mut command = topology_command(false, &paths, &state)?;
			arm_owner_failure(failure);
			arm_owner_transition_probe();
			arm_spawn_cleanup_handle_probe();

			let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
			let failure_was_not_consumed = clear_owner_failure();
			let primary_preserved = primary_owner_failure_preserved(outcome, &identity, was_panic);
			let (transitioned, allocator_callback, later_operation) =
				finish_owner_transition_probe();
			assert!(!transitioned, "the native policy transition must not run");
			assert!(!allocator_callback);
			assert!(!later_operation);
			assert_eq!(spawn_cleanup_handle_close_count(), 1);
			assert_eq!(finish_spawn_cleanup_handle_probe(), 1);
			assert!(
				!failure_was_not_consumed,
				"the final owner hook did not consume its injected failure"
			);
			assert!(
				primary_preserved,
				"armed cleanup replaced the final owner failure"
			);
			assert_tree_terminated(&paths, &state)?;
			std::fs::remove_file(&paths.descendant_pid)?;

			let mut child = command.spawn().expect("the command remains reusable");
			child.start_kill()?;
			let _ = child.wait().await?;
			drop(child);
			assert_tree_terminated(&paths, &state)?;
		}
		Ok(())
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
	async fn final_owner_transition_has_no_later_test_or_caller_operation() -> Result<()> {
		for provider in [false, true] {
			let directory = tempfile::tempdir()?;
			let paths = TreePaths::new(directory.path());
			let state = Arc::new(LifecycleState::default());
			let slot_calls = Arc::new(AtomicUsize::new(0));
			let events = Arc::new(std::sync::Mutex::new(Vec::with_capacity(2)));
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
				arm_owner_transition_probe();
				arm_owner_events(Arc::clone(&events));
				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				let (transitioned, allocator_callback, later_operation) =
					finish_owner_transition_probe();
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
				assert!(transitioned, "the native final-owner transition completed");
				assert!(
					!allocator_callback,
					"an allocator callback ran after the native final-owner transition"
				);
				assert!(
					!later_operation,
					"a caller or test operation ran after the native final-owner transition"
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
					["slot-accessor", "before-owner-transition"],
					"the final native transition follows the last caller-defined accessor"
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
