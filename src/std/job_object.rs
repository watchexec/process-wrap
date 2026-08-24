use std::{
	any::Any,
	io::{Error, ErrorKind, Result},
	os::windows::io::{AsRawHandle, BorrowedHandle},
	process::ExitStatus,
	time::Duration,
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
		JobPort, job_creation_flags, make_job_object, resume_threads, set_job_kill_on_drop,
		terminate_job, wait_on_job,
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
			unsafe { CloseHandle(its.completion_port.0) }.ok();
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
		if let ChildExitStatus::Exited(status) = &self.exit_status {
			return Ok(*status);
		}

		// always wait for parent to exit first, as by the time it does,
		// it's likely that all its children have already exited.
		let status = self.inner.wait()?;
		self.exit_status = ChildExitStatus::Exited(status);

		// nevertheless, now wait and make sure we reap all children.
		let JobPort {
			completion_port, ..
		} = self.job_port;
		let _ = wait_on_job(completion_port, None)?;
		Ok(status)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		let _ = wait_on_job(self.job_port.completion_port, Some(Duration::ZERO))?;
		self.inner.try_wait()
	}
}
