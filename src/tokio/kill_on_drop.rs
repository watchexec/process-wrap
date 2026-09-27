use std::io::Result;

use super::{CommandWrap, CommandWrapper, SpawnAttempt};

/// Portable kill-on-drop policy wrapper for a [`Command`](super::Command).
///
/// Calling the native-shaped `Command::kill_on_drop` method makes the command native-only because the
/// setting cannot be queried afterward. This wrapper instead records the policy on each `SpawnAttempt`,
/// allowing `JobObject` and alternate spawn providers to preserve it.
///
/// On Unix, dropping a process-group or session child still applies Tokio's kill-on-drop behavior to
/// the direct native child only. Use the group-aware child's explicit kill or signal methods when the
/// entire process group must be targeted. On Windows, combining this wrapper with `JobObject` in either
/// registration order enables kill-on-last-job-handle-close for every process still associated with
/// that job while the JobObject layer remains installed. Consuming that layer relinquishes whole-job
/// supervision and leaves only the lower direct-child policy.
#[derive(Clone, Copy, Debug)]
pub struct KillOnDrop;

impl CommandWrapper for KillOnDrop {
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _core: &CommandWrap) -> Result<()> {
		attempt.set_kill_on_drop(true);
		Ok(())
	}
}
