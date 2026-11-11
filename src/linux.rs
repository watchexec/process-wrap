//! Linux API support functions.

use std::io::{Error, Result};

use nix::sys::signal::Signal;

/// Build the `pre_exec` hook which ties a child to the thread that spawns it.
///
/// Call this on the thread which then spawns the child: it captures that thread's IDs, as inside
/// the hook (in the child) `gettid()` would return the child's own.
///
/// The hook is sound to pass to `pre_exec`, which requires it to be sound to run in the forked
/// child, after `fork()` and before `exec()`. At that point the process may have been forked from a
/// multi-threaded program, so any state guarded by a mutex or allocation in another thread is
/// frozen in an arbitrary state. The hook must therefore be async-signal-safe: no allocations, no
/// locks, no calls into logic that may touch inconsistent global state. This one is:
///   * it captures only `Copy` integers (the death signal, tgid and tid), with no shared state, no
///     heap references and no locks;
///   * it performs only async-signal-safe raw syscalls (`prctl`, `tgkill`);
///   * on failure it converts `Errno` into `io::Error`, which for OS errors stores the errno inline
///     and does not allocate;
///   * it cannot panic: the error paths return `Err` rather than unwinding.
pub(crate) fn thread_death_hook(
	death_signal: Signal,
) -> impl FnMut() -> Result<()> + Send + Sync + 'static {
	let parent_tgid = nix::unistd::getpid().as_raw();
	let parent_tid = nix::unistd::gettid().as_raw();

	move || {
		// Tie the child to the parent: request the death signal when the parent thread dies.
		nix::sys::prctl::set_pdeathsig(Some(death_signal)).map_err(Error::from)?;

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
			return Err(Error::from_raw_os_error(nix::libc::ECANCELED));
		}
		Ok(())
	}
}
