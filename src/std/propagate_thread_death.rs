use std::io::Result;

use nix::sys::signal::Signal;

use super::{CommandWrap, CommandWrapper};
use crate::linux::thread_death_hook;

/// Asks the kernel to send `SIGTERM` to the child process when the *thread* that spawned it exits.
/// See [prctrl](https://www.man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html).
///
/// This wrapper is only available on Linux.
///
/// # Caveats
/// - The setting is not inherited by grandchildren, is cleared when the child
///   execs a setuid/setgid or file-capability binary, and the default signal `SIGTERM` can be
///   caught or ignored by the child.
///
/// # Examples
///
/// ```rust,no_run
/// use std::{io, thread, process::{Stdio, Command}};
/// use process_wrap::std::{CommandWrap, PropagateThreadDeath};
///
/// // PropagateThreadDeath ties the subprocess lifetime to the spawning thread.
/// let handle = thread::spawn(|| {
///     let child = CommandWrap::from(Command::new("yes"))
///     .wrap(PropagateThreadDeath::default())
///     .spawn()?;
///     Ok::<u32, io::Error>(child.id())
/// });
/// let dead_pid = handle
/// .join()
/// .expect("Thread panicked")
/// .expect("Thread failed to spawn");
/// // The `yes` subprocess was killed by the kernel.
/// ```
///
/// Overriding the default `SIGTERM` signal:
///
/// ```rust
/// use process_wrap::std::{CommandWrap, PropagateThreadDeath};
/// use nix::sys::signal::SIGKILL;
///
/// let mut command = CommandWrap::with_new("ls", |_cmd| {});
/// command.wrap(PropagateThreadDeath::new(SIGKILL as _)?);
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct PropagateThreadDeath {
	death_signal: Signal,
}

impl Default for PropagateThreadDeath {
	fn default() -> Self {
		Self {
			death_signal: Signal::SIGTERM,
		}
	}
}

impl PropagateThreadDeath {
	/// Creates a new [`Self`] with a custom signal to send to the child process.
	/// This overrides the default `SIGTERM` signal.
	///
	/// Fails if `death_signal` isn't a valid signal number.
	pub fn new(death_signal: i32) -> Result<Self> {
		Ok(Self {
			death_signal: Signal::try_from(death_signal)?,
		})
	}
}

impl CommandWrapper for PropagateThreadDeath {
	fn pre_spawn(
		&mut self,
		command: &mut std::process::Command,
		_core: &CommandWrap,
	) -> Result<()> {
		use std::os::unix::process::CommandExt as _;

		// Built here, on the thread which spawns the child.
		let hook = thread_death_hook(self.death_signal);

		#[expect(unsafe_code)]
		// SAFETY: the hook is sound to run between `fork()` and `exec()`; see `thread_death_hook`.
		unsafe {
			command.pre_exec(hook);
		}

		Ok(())
	}
}
