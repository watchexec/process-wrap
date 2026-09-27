use std::io::Result;

#[cfg(feature = "tracing")]
use tracing::instrument;

use super::{CommandWrap, CommandWrapper, PendingChildWrapper, SpawnAttempt, core::ChildWrapper};

/// Wrapper which creates a new session and group for the `Command`.
///
/// This wrapper is only available on Unix.
///
/// It creates a new session and new process group and sets the [`Command`](super::Command) as its leader.
/// See [setsid(2)](https://pubs.opengroup.org/onlinepubs/9699919799/functions/setsid.html).
///
/// You may find that some programs behave differently or better when running in a session rather
/// than a process group, or vice versa.
///
/// This wrapper uses [the same child wrapper as `ProcessGroup`](super::ProcessGroupChild) and does
/// the same setup (plus the session setup); using both together is unnecessary and may misbehave.
/// With the `Pty` wrapper, the terminal provider performs the required session setup while this
/// wrapper retains group-wide signalling until the direct child exits. Waiting still follows that
/// child. Explicitly combining both supervision wrappers is invalid for that transport.
#[derive(Clone, Copy, Debug)]
pub struct ProcessSession;

impl CommandWrapper for ProcessSession {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_process_session()
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		inner: &mut dyn ChildWrapper,
		_core: &CommandWrap,
	) -> Result<Option<PendingChildWrapper>> {
		let spawned_id = inner.try_spawned_id();
		super::ProcessGroup::leader()
			.detached_child(inner, spawned_id)
			.map(Some)
	}

	fn wrap_child_with_spawned_id(
		&mut self,
		inner: &mut dyn ChildWrapper,
		spawned_id: Option<u32>,
		_core: &CommandWrap,
	) -> Result<Option<PendingChildWrapper>> {
		super::ProcessGroup::leader()
			.detached_child(inner, spawned_id)
			.map(Some)
	}
}
