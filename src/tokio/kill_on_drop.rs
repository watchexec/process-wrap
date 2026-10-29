use std::io::Result;

use tokio::process::Command;

use super::{CommandWrap, CommandWrapper};

/// Shim wrapper which sets kill-on-drop on a [`Command`].
///
/// This wrapper exists to be able to set the kill-on-drop flag on a [`Command`] and also store that
/// fact in the wrapper, so that it can be used by other wrappers. Notably this is used by the
/// `JobObject` wrapper.
///
/// # Example
///
/// ```rust,no_run
/// use tokio::process::Command;
/// use process_wrap::tokio::{CommandWrap, KillOnDrop};
///
/// # #[tokio::main]
/// # async fn main() -> std::io::Result<()> {
/// let mut child = CommandWrap::from(Command::new("yes"))
/// .wrap(KillOnDrop)
/// .spawn()?;
/// let dead_pid = child.id();
/// drop(child);
/// // The `yes` subprocess will be killed by the tokio runtime.
/// # Ok(()) }
/// ```
#[derive(Clone, Copy, Debug)]
pub struct KillOnDrop;

impl CommandWrapper for KillOnDrop {
	fn pre_spawn(&mut self, command: &mut Command, _core: &CommandWrap) -> Result<()> {
		command.kill_on_drop(true);
		Ok(())
	}
}
