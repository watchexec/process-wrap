use std::io::Result;

use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;

use super::{CommandWrap, CommandWrapper, SpawnAttempt};

/// Portable wrapper for Windows process creation flags.
///
/// This wrapper is only available on Windows. Calling the native-shaped `Command::creation_flags`
/// method makes the command native-only because those flags cannot be queried afterward. This wrapper
/// instead records them on each `SpawnAttempt`, allowing `JobObject` and alternate spawn providers to
/// preserve and inspect the policy.
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
