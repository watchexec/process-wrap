use super::CommandWrapper;

/// Opt in to terminating every process in a Windows [`super::JobObject`] when its child wrapper is
/// dropped.
///
/// This marker has no effect without [`super::JobObject`]. Without this marker, the std
/// [`super::JobObject`] keeps its existing behavior and closing its child wrapper does not
/// terminate the job.
#[derive(Clone, Copy, Debug)]
pub struct KillOnDrop;

impl CommandWrapper for KillOnDrop {}
