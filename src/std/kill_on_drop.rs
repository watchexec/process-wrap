use std::{io::Result, process::Command};

use super::{StdCommandWrap, StdCommandWrapper};

#[derive(Debug)]
pub struct KillOnDrop;

impl StdCommandWrapper for KillOnDrop {
	fn pre_spawn(&mut self, command: &mut Command, _core: &StdCommandWrap) -> Result<()> {
		command.kill_on_drop(true);
		Ok(())
	}
}
