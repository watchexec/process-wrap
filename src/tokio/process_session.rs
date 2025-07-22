use std::io::{Error, Result};

use nix::unistd::{setsid, Pid};
use tokio::process::Command;
#[cfg(feature = "tracing")]
use tracing::instrument;

use super::{CommandWrap, CommandWrapper};

/// Wrapper which creates a new session and group for the `Command`.
///
/// This wrapper is only available on Unix.
///
/// It creates a new session and new process group and sets the [`Command`] as its leader.
/// See [setsid(2)](https://pubs.opengroup.org/onlinepubs/9699919799/functions/setsid.html).
///
/// You may find that some programs behave differently or better when running in a session rather
/// than a process group, or vice versa.
///
/// This wrapper uses [the same child wrapper as `ProcessGroup`](super::ProcessGroupChild) and does
/// the same setup (plus the session setup); using both together is unnecessary and may misbehave.
#[derive(Clone, Copy, Debug)]
pub struct ProcessSession;

impl CommandWrapper for ProcessSession {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, command: &mut Command, _core: &CommandWrap) -> Result<()> {
		unsafe {
			command.pre_exec(move || setsid().map_err(Error::from).map(|_| ()));
		}

		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		inner: Box<dyn super::core::ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn super::core::ChildWrapper>> {
		let pgid = Pid::from_raw(
			i32::try_from(
				inner
					.id()
					.expect("Command was reaped before we could read its PID"),
			)
			.expect("Command PID > i32::MAX"),
		);

		Ok(Box::new(super::ProcessGroupChild::new(inner, pgid)))
	}
}
