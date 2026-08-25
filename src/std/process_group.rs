use std::{
	io::{Error, Result},
	process::ExitStatus,
};

use nix::{
	sys::signal::{Signal, killpg},
	unistd::Pid,
};
#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::{ChildExitStatus, unix::ProcessGroupTarget};

use super::{ChildWrapper, CommandWrap, CommandWrapper, SpawnAttempt};

/// Wrapper which sets the process group of a [`Command`](super::Command).
///
/// This wrapper is only available on Unix.
///
/// It sets the process group of a [`Command`](super::Command), either to itself as the leader of a new group, or to
/// an existing one by its PGID. See [setpgid(2)](https://pubs.opengroup.org/onlinepubs/9699919799/functions/setpgid.html).
///
/// Process groups direct signals to all members of the group, and also serve to control job
/// placement in foreground or background, among other actions.
///
/// This wrapper provides a child wrapper: [`ProcessGroupChild`].
#[derive(Clone, Copy, Debug)]
pub struct ProcessGroup {
	target: ProcessGroupTarget,
}

impl ProcessGroup {
	/// Create a process group wrapper setting up a new process group with the command as the leader.
	pub fn leader() -> Self {
		Self {
			target: ProcessGroupTarget::Leader,
		}
	}

	/// Create a process group wrapper attaching the command to an existing process group ID.
	pub fn attach_to(leader: u32) -> Self {
		Self {
			target: ProcessGroupTarget::AttachTo(leader),
		}
	}
}

/// Wrapper for `Child` which signals the process group while its direct child is live.
///
/// Waiting follows the direct child. Process-wrap deliberately stops using a numeric process-group ID
/// after that child has been reaped, because the operating system may immediately reuse the ID for an
/// unrelated group.
#[derive(Debug)]
pub struct ProcessGroupChild {
	inner: Box<dyn ChildWrapper>,
	exit_status: ChildExitStatus,
	pgid: Pid,
}

impl ProcessGroupChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
	pub(crate) fn new(
		inner: Box<dyn ChildWrapper>,
		pgid: Pid,
		exit_status: Option<ExitStatus>,
	) -> Self {
		Self {
			inner,
			exit_status: exit_status.map_or(ChildExitStatus::Running, ChildExitStatus::Exited),
			pgid,
		}
	}

	/// Get the process group ID of this child process.
	///
	/// See: [`man 'setpgid(2)'`](https://www.man7.org/linux/man-pages/man2/setpgid.2.html)
	pub fn pgid(&self) -> u32 {
		self.pgid.as_raw() as _
	}
}

impl CommandWrapper for ProcessGroup {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_process_group(self.target)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		mut inner: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let direct_pid = Pid::from_raw(i32::try_from(inner.id()).map_err(Error::other)?);
		let pgid = match self.target {
			ProcessGroupTarget::Leader => direct_pid,
			ProcessGroupTarget::AttachTo(pgid) => Pid::from_raw(
				i32::try_from(pgid).expect("process group IDs are validated before spawning"),
			),
		};
		let exit_status = inner.try_wait()?;

		Ok(Box::new(ProcessGroupChild::new(inner, pgid, exit_status)))
	}
}

impl ProcessGroupChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn signal_imp(&self, sig: Signal) -> Result<()> {
		if matches!(self.exit_status, ChildExitStatus::Exited(_)) {
			return Ok(());
		}
		killpg(self.pgid, sig).map_err(Error::from)
	}
}

impl ChildWrapper for ProcessGroupChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		if matches!(self.exit_status, ChildExitStatus::Running)
			&& let Some(status) = self.inner.try_wait()?
		{
			self.exit_status = ChildExitStatus::Exited(status);
		}
		self.signal_imp(Signal::SIGKILL)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Result<ExitStatus> {
		match self.exit_status {
			ChildExitStatus::Running => {
				let status = self.inner.wait()?;
				self.exit_status = ChildExitStatus::Exited(status);
				Ok(status)
			}
			ChildExitStatus::Exited(status) => Ok(status),
		}
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		match self.exit_status {
			ChildExitStatus::Running => {
				let status = self.inner.try_wait()?;
				if let Some(status) = status {
					self.exit_status = ChildExitStatus::Exited(status);
				}
				Ok(status)
			}
			ChildExitStatus::Exited(status) => Ok(Some(status)),
		}
	}

	fn signal(&self, sig: i32) -> Result<()> {
		self.signal_imp(Signal::try_from(sig)?)
	}
}
