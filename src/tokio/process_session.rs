use std::io::{Error, ErrorKind, Result};

use nix::unistd::Pid;
#[cfg(feature = "tracing")]
use tracing::instrument;

use super::{CommandWrap, CommandWrapper, SpawnAttempt};

/// Wrapper which creates a new session and group for the `Command`.
///
/// This wrapper is only available on Unix.
///
/// It creates a new session and new process group and sets the [`Command`](super::Command) as its leader.
/// See [setsid(2)](https://pubs.opengroup.org/onlinepubs/9699919799/functions/setsid.html).
///
/// You may find that some programs behave differently or better when running in a session rather
/// than a process group, or vice versa.
///
/// This wrapper uses [the same child wrapper as `ProcessGroup`](super::ProcessGroupChild) and does
/// the same setup (plus the session setup); using both together is unnecessary and may misbehave.
/// With the `Pty` wrapper, the terminal provider performs the required session setup while this
/// wrapper retains group-aware child supervision. Explicitly combining both supervision wrappers is
/// invalid for that transport.
#[derive(Clone, Copy, Debug)]
pub struct ProcessSession;

impl CommandWrapper for ProcessSession {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_process_session()
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		mut inner: Box<dyn super::core::ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn super::core::ChildWrapper>> {
		let mut direct_id = inner.id();
		#[cfg(feature = "pty")]
		if direct_id.is_none() {
			direct_id = inner.try_spawned_id();
		}
		let direct_id = direct_id.ok_or_else(|| {
			Error::new(
				ErrorKind::InvalidInput,
				"the child exited before session supervision could retain its PID",
			)
		})?;
		let direct_pid = Pid::from_raw(i32::try_from(direct_id).map_err(Error::other)?);
		let exit_status = inner.try_wait()?;

		Ok(Box::new(super::ProcessGroupChild::new(
			inner,
			direct_pid,
			exit_status,
		)))
	}
}
