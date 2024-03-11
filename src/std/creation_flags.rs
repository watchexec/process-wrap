use std::{io::Result, os::windows::process::CommandExt, process::Command};

use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;

use super::{StdCommandWrap, StdCommandWrapper};

#[derive(Debug, Clone)]
pub struct CreationFlags(pub PROCESS_CREATION_FLAGS);

impl StdCommandWrapper for CreationFlags {
	fn pre_spawn(&mut self, command: &mut Command, _core: &StdCommandWrap) -> Result<()> {
		command.creation_flags((self.0).0);
		Ok(())
	}
}
