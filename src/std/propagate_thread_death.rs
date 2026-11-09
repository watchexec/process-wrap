use std::io::Result;

use nix::sys::signal::Signal;

use super::{CommandWrap, CommandWrapper};

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
/// command.wrap(PropagateThreadDeath::new(Some(SIGKILL as _))?);
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct PropagateThreadDeath {
	death_signal: Option<Signal>,
}

impl Default for PropagateThreadDeath {
	fn default() -> Self {
		Self {
			death_signal: Some(nix::sys::signal::SIGTERM),
		}
	}
}

impl PropagateThreadDeath {
	/// Creates a new [`Self`] with a custom signal to send to the child process.
	/// This overrides the default `SIGTERM` signal.
	pub fn new(death_signal: Option<i32>) -> Result<Self> {
		let death_signal = death_signal.map(Signal::try_from).transpose()?;
		Ok(Self { death_signal })
	}
}

impl CommandWrapper for PropagateThreadDeath {
	fn pre_spawn(
		&mut self,
		command: &mut std::process::Command,
		_core: &CommandWrap,
	) -> Result<()> {
		use std::os::unix::process::CommandExt as _;

		// Captured in the spawning thread, before the fork. Inside `pre_exec` we
		// are in the child, where `gettid()` would return the child's tid. These
		// are plain integers, so the closure below stays allocation-free and
		// captures no shared state.
		let parent_tgid = nix::unistd::getpid().as_raw();
		let parent_tid = nix::unistd::gettid().as_raw();
		let death_signal = self.death_signal;

		let pre_exec = move || {
			// Tie the child to the parent: request the death signal when the parent thread dies.
			nix::sys::prctl::set_pdeathsig(death_signal).map_err(std::io::Error::from)?;

			// The spawning *thread* may have exited between fork and prctl, in which
			// case the kernel never queued our death signal. Signal 0 delivers nothing and
			// only checks that the exact thread (tgid + tid) still exists; it fails
			// with ESRCH if the thread is gone. We report `ECANCELED` instead so the
			// error cannot be mistaken for a missing executable.
			#[expect(unsafe_code)]
			// SAFETY: thin wrapper over the `tgkill` syscall with plain integer
			// arguments; async-signal-safe.
			let rc = unsafe { nix::libc::tgkill(parent_tgid, parent_tid, 0) };
			if rc == -1 {
				return Err(std::io::Error::from_raw_os_error(nix::libc::ECANCELED));
			}
			Ok(())
		};

		#[expect(unsafe_code)]
		// SAFETY: `CommandExt::pre_exec` requires the closure to be sound to run in
		// the forked child, i.e. after `fork()`, before `exec()`. At that point the
		// process may have been forked from a multi-threaded program, so any state
		// guarded by a mutex/allocation in another thread is frozen in an
		// arbitrary state. The closure must therefore be async-signal-safe: no
		// allocations, no locks, no calls into logic that may touch inconsistent
		// global state.
		//
		// The closure installed here satisfies that requirement:
		//   * it captures only `Copy` integers (death_signal, tgid and tid), with no shared
		//     state, no heap references and no locks;
		//   * it performs only async-signal-safe raw syscalls (`prctl`, `tgkill`);
		//   * on failure it converts `Errno` into `io::Error`, which for OS errors
		//     stores the errno inline and does not allocate;
		//   * it cannot panic: the error paths return `Err` rather than unwinding.
		//
		// Since the closure meets `pre_exec`'s invariants, calling
		// `Command::pre_exec` on it is sound.
		unsafe {
			command.pre_exec(pre_exec);
		}

		Ok(())
	}
}
