use std::{
	io::{Error, Result},
	process::ExitStatus,
};

use nix::{sys::signal::Signal, unistd::Pid};
#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::{ChildExitStatus, unix::ProcessGroupTarget};

use super::{
	ChildWrapper, ChildWrapperLayer, ChildWrapperSlots, CommandWrap, CommandWrapper,
	PendingChildWrapper, SpawnAttempt,
};

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

	pub(super) fn detached_child(
		self,
		inner: &mut dyn ChildWrapper,
		spawned_id: Option<u32>,
	) -> Result<PendingChildWrapper> {
		if !inner.has_process_group_signal_capability() {
			return Err(Error::new(
				std::io::ErrorKind::Unsupported,
				"the child does not expose liveness-gated process-group signalling",
			));
		}
		let direct_id = spawned_id.ok_or_else(|| {
			Error::new(
				std::io::ErrorKind::InvalidInput,
				"the spawn transport did not retain its historical process ID",
			)
		})?;
		let direct_pid = Pid::from_raw(i32::try_from(direct_id).map_err(Error::other)?);
		let pgid = match self.target {
			ProcessGroupTarget::Leader => direct_pid,
			ProcessGroupTarget::AttachTo(pgid) => Pid::from_raw(
				i32::try_from(pgid).expect("process group IDs are validated before spawning"),
			),
		};
		let exit_status = inner.try_wait()?;
		Ok(PendingChildWrapper::new(ProcessGroupChild::detached(
			pgid,
			exit_status,
		)))
	}
}

/// Wrapper for `Child` which signals the process group while its direct child is live.
///
/// Waiting follows the direct child. Process-wrap deliberately stops using a numeric process-group ID
/// after that child has been reaped, because the operating system may immediately reuse the ID for an
/// unrelated group.
#[derive(Debug)]
pub struct ProcessGroupChild {
	inner: Option<Box<dyn ChildWrapper>>,
	exit_status: ChildExitStatus,
	pgid: Pid,
}

impl ProcessGroupChild {
	pub(crate) fn detached(pgid: Pid, exit_status: Option<ExitStatus>) -> Self {
		Self {
			inner: None,
			exit_status: exit_status.map_or(ChildExitStatus::Running, ChildExitStatus::Exited),
			pgid,
		}
	}

	fn inner_ref(&self) -> &dyn ChildWrapper {
		self.inner
			.as_deref()
			.expect("an installed process-group layer owns its child")
	}

	fn inner_mut_ref(&mut self) -> &mut dyn ChildWrapper {
		self.inner
			.as_deref_mut()
			.expect("an installed process-group layer owns its child")
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
		inner: &mut dyn ChildWrapper,
		_core: &CommandWrap,
	) -> Result<Option<PendingChildWrapper>> {
		let spawned_id = inner.try_spawned_id();
		self.detached_child(inner, spawned_id).map(Some)
	}

	fn wrap_child_with_spawned_id(
		&mut self,
		inner: &mut dyn ChildWrapper,
		spawned_id: Option<u32>,
		_core: &CommandWrap,
	) -> Result<Option<PendingChildWrapper>> {
		self.detached_child(inner, spawned_id).map(Some)
	}
}

impl ProcessGroupChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn signal_imp(&mut self, sig: Signal) -> Result<()> {
		if matches!(self.exit_status, ChildExitStatus::Exited(_)) {
			return Ok(());
		}
		let pgid = self.pgid.as_raw();
		let outcome = self
			.inner_mut_ref()
			.try_signal_process_group(pgid, sig as i32)
			.ok_or_else(|| {
				Error::new(
					std::io::ErrorKind::Unsupported,
					"the child lost its liveness-gated process-group signalling capability",
				)
			})??;
		if let Some(status) = outcome {
			self.exit_status = ChildExitStatus::Exited(status);
		}
		Ok(())
	}
}

impl ChildWrapperLayer for ProcessGroupChild {
	fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
		ChildWrapperSlots::new(&mut self.inner)
	}
}

impl ChildWrapper for ProcessGroupChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner_ref()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner_mut_ref()
	}
	fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
			.take()
			.expect("an installed process-group layer owns its child")
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		self.signal_imp(Signal::SIGKILL)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Result<ExitStatus> {
		match self.exit_status {
			ChildExitStatus::Running => {
				let status = self.inner_mut_ref().wait()?;
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
				let status = self.inner_mut_ref().try_wait()?;
				if let Some(status) = status {
					self.exit_status = ChildExitStatus::Exited(status);
				}
				Ok(status)
			}
			ChildExitStatus::Exited(status) => Ok(Some(status)),
		}
	}

	fn signal(&mut self, sig: i32) -> Result<()> {
		self.signal_imp(Signal::try_from(sig)?)
	}
}
