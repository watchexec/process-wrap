use std::io::Result;

use nix::sys::signal::Signal;
use tokio::process::Command;

use super::{CommandWrap, CommandWrapper};
use crate::linux::thread_death_hook;

/// Asks the kernel to send `SIGTERM` to the child process when the *thread* that spawned it exits.
/// See [prctl](https://www.man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html).
///
/// This wrapper is only available on Linux.
///
/// The child is tied to whichever thread calls `spawn()`, which under Tokio is a runtime thread:
/// - from a task on a multi-thread runtime, a worker thread, which lives as long as the runtime;
/// - from a current-thread runtime, the thread that runs it;
/// - from [`spawn_blocking`](tokio::task::spawn_blocking), a blocking pool thread, which the pool
///   retires once it has been idle for a while (10 seconds by default), signalling the child then;
/// - from [`block_in_place`](https://docs.rs/tokio/1/tokio/task/fn.block_in_place.html), a worker
///   thread which may move to the blocking pool afterwards, and be retired like one.
///
/// So outside of `spawn_blocking` and `block_in_place`, the child is signalled when the runtime
/// shuts down or the process dies, including when it is killed and can't clean up after itself.
///
/// # Caveats
/// - The setting is not inherited by grandchildren, is cleared when the child
///   execs a setuid/setgid or file-capability binary, and the default signal `SIGTERM` can be
///   caught or ignored by the child.
///
/// # Examples
///
/// ```rust,no_run
/// use std::{io, thread};
/// use tokio::process::Command;
/// use process_wrap::tokio::{CommandWrap, PropagateThreadDeath};
///
/// // PropagateThreadDeath ties the subprocess lifetime to the runtime's thread.
/// let handle = thread::spawn(|| {
///     let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
///     runtime.block_on(async {
///         let child = CommandWrap::from(Command::new("yes"))
///         .wrap(PropagateThreadDeath::default())
///         .spawn()?;
///         Ok::<_, io::Error>(child.id())
///     })
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
/// use process_wrap::tokio::{CommandWrap, PropagateThreadDeath};
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
	fn pre_spawn(&mut self, command: &mut Command, _core: &CommandWrap) -> Result<()> {
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
