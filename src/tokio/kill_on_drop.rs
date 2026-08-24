use std::io::Result;

use super::{CommandWrap, CommandWrapper, SpawnAttempt};

/// Shim wrapper which sets kill-on-drop on a [`Command`](super::Command).
///
/// This wrapper exists to be able to set the kill-on-drop flag on a `Command` and also store that
/// fact in the wrapper, so that it can be used by other wrappers. Notably this is used by the
/// `JobObject` wrapper.
#[derive(Clone, Copy, Debug)]
pub struct KillOnDrop;

impl CommandWrapper for KillOnDrop {
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_kill_on_drop(true);
		Ok(())
	}
}
