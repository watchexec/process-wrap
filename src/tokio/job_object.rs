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
	job_drained: bool,
	poll_cadence: Option<tokio::task::JoinHandle<()>>,
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
			poll_cadence: None,
			job_port,
			final_kill_on_drop,
			spawn_finalized: false,
		}
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
			let status = match self.exit_status {
				ChildExitStatus::Running => {
					// Cache direct-child exit independently from whole-job drain. There is no await
					// between observing the status and retaining it, so cancellation cannot lose it.
					let status = self.inner.wait().await?;
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
				if poll_job_drain(
					self.job_port.job,
					self.job_port.completion_port.as_handle(),
					Duration::ZERO,
				)?
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
			self.clear_poll_cadence();
		}

		match (self.exit_status, self.job_drained) {
			(ChildExitStatus::Exited(status), true) => Ok(Some(status)),
			_ => Ok(None),
		}
	}
}
