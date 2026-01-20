use std::{future::Future, io::Result, pin::Pin, process::ExitStatus, time::Duration};

use tokio::{process::Command, task::spawn_blocking};
#[cfg(feature = "tracing")]
use tracing::{debug, instrument};
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE},
	System::Threading::CREATE_SUSPENDED,
};

use crate::{
	ChildExitStatus,
	windows::{JobPort, make_job_object, resume_threads, terminate_job, wait_on_job},
};

#[cfg(feature = "creation-flags")]
use super::CreationFlags;
#[cfg(feature = "kill-on-drop")]
use super::KillOnDrop;
use super::{ChildWrapper, CommandWrap, CommandWrapper};

/// Wrapper which creates a job object context for a `Command`.
///
/// This wrapper is only available on Windows.
///
/// It creates a Windows Job Object and associates the [`Command`] to it. This behaves analogously
/// to process groups on Unix or even cgroups on Linux, with the ability to restrict resource use.
/// See [Job Objects](https://docs.microsoft.com/en-us/windows/win32/procthread/job-objects).
///
/// This wrapper provides a child wrapper: [`JobObjectChild`].
///
/// When both [`CreationFlags`] and [`JobObject`] are used together, either:
/// - `CreationFlags` must come first, or
/// - `CreationFlags` must include `CREATE_SUSPENDED`
#[derive(Clone, Copy, Debug)]
pub struct JobObject;

impl CommandWrapper for JobObject {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, command: &mut Command, core: &CommandWrap) -> Result<()> {
		let mut flags = CREATE_SUSPENDED;
		#[cfg(feature = "creation-flags")]
		if let Some(CreationFlags(user_flags)) = core.get_wrap::<CreationFlags>() {
			flags |= *user_flags;
		}

		command.creation_flags(flags.0);
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		inner: Box<dyn ChildWrapper>,
		core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		#[cfg(feature = "kill-on-drop")]
		let kill_on_drop = core.has_wrap::<KillOnDrop>();
		#[cfg(not(feature = "kill-on-drop"))]
		let kill_on_drop = false;

		#[cfg(feature = "creation-flags")]
		let create_suspended = core
			.get_wrap::<CreationFlags>()
			.map_or(false, |flags| flags.0.contains(CREATE_SUSPENDED));
		#[cfg(not(feature = "creation-flags"))]
		let create_suspended = false;

		#[cfg(feature = "tracing")]
		debug!(
			?kill_on_drop,
			?create_suspended,
			"options from other wrappers"
		);

		let handle = HANDLE(
			inner
				.inner_child()
				.raw_handle()
				.expect("child has exited but it has not even started") as _,
		);

		let job_port = make_job_object(handle, kill_on_drop)?;

		// only resume if the user didn't specify CREATE_SUSPENDED
		if !create_suspended {
			resume_threads(handle)?;
		}

		Ok(Box::new(JobObjectChild::new(inner, job_port)))
	}
}

/// Wrapper for `Child` which waits on all processes within the job.
#[derive(Debug)]
pub struct JobObjectChild {
	inner: Box<dyn ChildWrapper>,
	exit_status: ChildExitStatus,
	job_port: JobPort,
}

impl JobObjectChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(job_port)))]
	pub(crate) fn new(inner: Box<dyn ChildWrapper>, job_port: JobPort) -> Self {
		Self {
			inner,
			exit_status: ChildExitStatus::Running,
			job_port,
		}
	}
}

impl ChildWrapper for JobObjectChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.inner()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.inner_mut()
	}
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		// manually drop the completion port
		let its = std::mem::ManuallyDrop::new(self.job_port);
		unsafe { CloseHandle(its.completion_port.0) }.ok();
		// we leave the job handle unclosed, otherwise the Child is useless
		// (as closing it will terminate the job)

		self.inner.into_inner()
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
				if wait_on_job(self.job_port.completion_port, Some(Duration::ZERO))?.is_break() {
					return Ok(status);
				}
			}

			// ...finally, if there are some that are still alive,
			// block in the background to reap them fully.
			let JobPort {
				completion_port, ..
			} = self.job_port;
			let _ = spawn_blocking(move || wait_on_job(completion_port, None)).await??;
			Ok(status)
		})
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		let _ = wait_on_job(self.job_port.completion_port, Some(Duration::ZERO))?;
		self.inner.try_wait()
	}
}
