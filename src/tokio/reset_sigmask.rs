use std::io::Result;

use nix::sys::signal::{sigprocmask, SigSet, SigmaskHow};
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
		unsafe {
			command.pre_exec(|| {
				let mut oldset = SigSet::empty();
				let newset = SigSet::all();

				#[cfg(feature = "tracing")]
				trace!(unblocking=?newset, "resetting process sigmask");

				sigprocmask(SigmaskHow::SIG_UNBLOCK, Some(&newset), Some(&mut oldset))?;

				#[cfg(feature = "tracing")]
				trace!(?oldset, "sigmask reset");
				Ok(())
			});
		}
		Ok(())
	}
}
