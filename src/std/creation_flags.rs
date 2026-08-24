use std::io::Result;

use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;

use super::{CommandWrap, CommandWrapper, SpawnAttempt};

/// Shim wrapper which sets Windows process creation flags.
///
/// This wrapper is only available on Windows.
///
/// It exists to be able to set creation flags on a `Command` and also store them in the wrapper, so
/// that they are not overwritten by other wrappers. Notably this is the only way to use creation
/// flags and the `JobObject` wrapper together.
///
/// When both `CreationFlags` and `JobObject` are used, process-wrap preserves these flags while
/// temporarily adding `CREATE_SUSPENDED`; registration order does not matter.
#[derive(Clone, Copy, Debug)]
pub struct CreationFlags(pub PROCESS_CREATION_FLAGS);

impl CommandWrapper for CreationFlags {
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_windows_creation_flags(self.0.0);
		Ok(())
	}
}
