use std::io::Result;

use tokio::process::Command;

use super::{TokioCommandWrap, TokioCommandWrapper};

#[derive(Debug, Clone)]
pub struct KillOnDrop;

impl TokioCommandWrapper for KillOnDrop {
	fn pre_spawn(&mut self, command: &mut Command, _core: &TokioCommandWrap) -> Result<()> {
		command.kill_on_drop(true);
		Ok(())
	}
}
