use std::io::Result;

#[cfg(feature = "tracing")]
use tracing::trace;

use super::{CommandWrap, CommandWrapper, SpawnAttempt};

/// Wrapper which resets the process signal mask.
///
/// By default a Command on Unix inherits its parent's [signal mask]. However, in some cases this
/// is not what you want. This wrapper resets the command's sigmask by unblocking all signals.
#[derive(Clone, Copy, Debug)]
pub struct ResetSigmask;

impl CommandWrapper for ResetSigmask {
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		#[cfg(feature = "tracing")]
		trace!("configuring process sigmask reset");
		attempt.set_reset_sigmask();
		Ok(())
	}
}
