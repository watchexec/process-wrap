use std::io::Result;

use nix::sys::signal::{SigSet, SigmaskHow, sigprocmask};
use tokio::process::Command;
#[cfg(feature = "tracing")]
use tracing::trace;

use super::{CommandWrap, CommandWrapper};

/// Wrapper which resets the process signal mask.
///
/// By default a Command on Unix inherits its parent's [signal mask]. However, in some cases this
/// is not what you want. This wrapper resets the command's sigmask by unblocking all signals.
#[derive(Clone, Copy, Debug)]
pub struct ResetSigmask;

impl CommandWrapper for ResetSigmask {
	fn pre_spawn(&mut self, command: &mut Command, _core: &CommandWrap) -> Result<()> {
		#[cfg(feature = "tracing")]
		trace!("registering child sigmask reset");

		unsafe {
			command.pre_exec(|| {
				let newset = SigSet::all();
				sigprocmask(SigmaskHow::SIG_UNBLOCK, Some(&newset), None)?;
				Ok(())
			});
		}
		Ok(())
	}
}
