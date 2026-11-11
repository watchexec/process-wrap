//! Linux API support functions.

use std::{
	io::{Error, Result},
	sync::{
		Arc,
		atomic::{AtomicI32, Ordering},
	},
};

use nix::sys::signal::Signal;

/// The thread about to spawn a child, recorded for a `PropagateThreadDeath` hook to check.
///
/// A command keeps its `pre_exec` hooks across spawns, so the wrapper installs its hook once and
/// records the spawning thread before each spawn, rather than installing a hook per spawn: the
/// hooks would pile up, and those from earlier spawns would check threads which may have exited.
///
/// A clone is for another command, so it starts afresh, without a hook.
#[derive(Debug, Default)]
pub(crate) struct SpawningThread(Option<Arc<ThreadIds>>);

#[derive(Debug, Default)]
struct ThreadIds {
	tgid: AtomicI32,
	tid: AtomicI32,
}

impl Clone for SpawningThread {
	fn clone(&self) -> Self {
		Self::default()
	}
}

impl SpawningThread {
	/// Record the calling thread as the one about to spawn the child.
	///
	/// Returns the hook to install with `pre_exec` the first time, and `None` after that, as the
	/// hook installed then reads what this records.
	pub(crate) fn prepare(
		&mut self,
		death_signal: Signal,
	) -> Option<impl FnMut() -> Result<()> + Send + Sync + 'static> {
		let (ids, new) = match &self.0 {
			Some(ids) => (ids.clone(), false),
			None => {
				let ids = Arc::new(ThreadIds::default());
				self.0 = Some(ids.clone());
				(ids, true)
			}
		};

		// Captured on this thread, before the fork: in the child, `gettid()` would return the
		// child's own.
		ids.tgid
			.store(nix::unistd::getpid().as_raw(), Ordering::Relaxed);
		ids.tid
			.store(nix::unistd::gettid().as_raw(), Ordering::Relaxed);

		new.then(|| thread_death_hook(ids, death_signal))
	}
}

/// Build the `pre_exec` hook which ties a child to the thread that spawns it.
///
/// The hook is sound to pass to `pre_exec`, which requires it to be sound to run in the forked
/// child, after `fork()` and before `exec()`. At that point the process may have been forked from a
/// multi-threaded program, so any state guarded by a mutex or allocation in another thread is
/// frozen in an arbitrary state. The hook must therefore be async-signal-safe: no allocations, no
/// locks, no calls into logic that may touch inconsistent global state. This one is:
///   * it captures only `Copy` values (the death signal) and an `Arc` it only reads atomic integers
///     through, which are lock-free loads; the child never touches the reference count, and the
///     spawning thread wrote the integers before forking, so the child's copy holds them;
///   * it performs only async-signal-safe raw syscalls (`prctl`, `tgkill`);
///   * on failure it converts `Errno` into `io::Error`, which for OS errors stores the errno inline
///     and does not allocate;
///   * it cannot panic: the error paths return `Err` rather than unwinding.
fn thread_death_hook(
	ids: Arc<ThreadIds>,
	death_signal: Signal,
) -> impl FnMut() -> Result<()> + Send + Sync + 'static {
	move || {
		let parent_tgid = ids.tgid.load(Ordering::Relaxed);
		let parent_tid = ids.tid.load(Ordering::Relaxed);

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
