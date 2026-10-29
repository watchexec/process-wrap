use std::io::Result;

use tokio::process::Command;
use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;

use super::{CommandWrap, CommandWrapper};

#[cfg(feature = "job-object")]
use super::JobObject;
#[cfg(feature = "job-object")]
use crate::windows::job_creation_flags;

/// Shim wrapper which sets Windows process creation flags.
///
/// This wrapper is only available on Windows.
///
/// It exists to be able to set creation flags on a [`Command`] and also store them in the wrapper, so
/// that they're no overwritten by other wrappers. Notably this is the only way to use creation
/// flags and the `JobObject` wrapper together.
///
/// When both [`CreationFlags`] and `JobObject` are used, process-wrap preserves these flags while
/// temporarily adding `CREATE_SUSPENDED`; registration order does not matter.
///
/// # Example
///
/// ```rust
/// use process_wrap::tokio::{CommandWrap, CreationFlags};
/// use windows::Win32::System::Threading::CREATE_NO_WINDOW;
///
/// let mut command = CommandWrap::with_new("ls", |command| { command.arg("-l"); });
/// command.wrap(CreationFlags(CREATE_NO_WINDOW.0));
/// ```
#[derive(Clone, Copy, Debug)]
pub struct CreationFlags(pub u32);

impl CommandWrapper for CreationFlags {
	fn pre_spawn(&mut self, command: &mut Command, core: &CommandWrap) -> Result<()> {
		let process_creation_flags = PROCESS_CREATION_FLAGS(self.0);
		#[cfg(feature = "job-object")]
		let flags = if core.has_wrap::<JobObject>() {
			job_creation_flags(process_creation_flags).flags
		} else {
			process_creation_flags
		};
		#[cfg(not(feature = "job-object"))]
		let flags = {
			let _ = core;
			process_creation_flags
		};

		command.creation_flags(flags.0);
		Ok(())
	}
}
