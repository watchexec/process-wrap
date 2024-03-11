use std::io::Result;

use tokio::process::Command;
use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;

use super::{TokioCommandWrap, TokioCommandWrapper};

#[derive(Debug, Clone)]
pub struct CreationFlags(pub PROCESS_CREATION_FLAGS);

impl TokioCommandWrapper for CreationFlags {
	fn pre_spawn(&mut self, command: &mut Command, _core: &TokioCommandWrap) -> Result<()> {
		command.creation_flags((self.0).0);
		Ok(())
	}
}
