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
	if child.start_kill().is_ok() {
		let _ = child.wait();
	}
}

#[derive(Debug)]
struct PreparedJobObject {
	job_port: JobPort,
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

		Ok(PreparedJobObject { job_port })
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
			false,
		)))
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self, inner)))]
	fn wrap_child(
		&mut self,
		mut inner: Box<dyn ChildWrapper>,
		core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let prepared = self.prepare_job(inner.as_mut(), core)?;
		let mut child = JobObjectChild::new(inner, prepared.job_port, false);
		child.disarm_job_object_layer()?;
		Ok(Box::new(child))
	}
}

/// Wrapper for `Child` which waits on all processes within the job.
#[derive(Debug)]
pub struct JobObjectChild {
	inner: Box<dyn ChildWrapper>,
	exit_status: ChildExitStatus,
	job_drained: bool,
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
			job_drained: false,
			job_port,
			final_kill_on_drop,
			spawn_finalized: false,
		}
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
	fn wait(&mut self) -> Result<ExitStatus> {
		let Self {
			inner,
			exit_status,
			job_drained,
			job_port,
			..
		} = self;
		wait_for_exit_and_job_drain_with(
			exit_status,
			job_drained,
			|| inner.wait(),
			|timeout| poll_job_drain(job_port.job, job_port.completion_port.as_handle(), timeout),
			Instant::now,
			std::thread::sleep,
			|_| {},
		)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		if matches!(self.exit_status, ChildExitStatus::Running) {
			let Some(status) = self.inner.try_wait()? else {
				return Ok(None);
			};
			self.exit_status = ChildExitStatus::Exited(status);
		}

		if !self.job_drained
			&& poll_job_drain(
				self.job_port.job,
				self.job_port.completion_port.as_handle(),
				Duration::ZERO,
			)?
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
		os::windows::process::ExitStatusExt,
		sync::mpsc,
		thread,
	};

	use super::*;

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
