use std::{io::Result, process::Command};

use super::{StdCommandWrap, StdCommandWrapper};

/// Shim wrapper which sets kill-on-drop on a `Command`.
///
/// This wrapper exists to be able to set the kill-on-drop flag on a `Command` and also store that
/// fact in the wrapper, so that it can be used by other wrappers. Notably this is used by the
/// `JobObject` wrapper.
#[derive(Debug)]
pub struct KillOnDrop;

impl StdCommandWrapper for KillOnDrop {
	fn pre_spawn(&mut self, command: &mut Command, _core: &StdCommandWrap) -> Result<()> {
		command.kill_on_drop(true);
		Ok(())
	}
}
