use super::prelude::*;

/// Spawn the same command twice, as wrappers' setup must hold up when it runs again.
fn spawn_twice(mut command: CommandWrap) -> Result<()> {
	for _ in 0..2 {
		let status = command.spawn()?.wait()?;
		assert!(status.success());
	}
	Ok(())
}

fn command() -> CommandWrap {
	CommandWrap::with_new("true", |_| {})
}

#[test]
fn nowrap() -> Result<()> {
	spawn_twice(command())
}

#[cfg(feature = "process-group")]
#[test]
fn process_group() -> Result<()> {
	let mut command = command();
	command.wrap(ProcessGroup::leader());
	spawn_twice(command)
}

#[cfg(feature = "process-session")]
#[test]
fn process_session() -> Result<()> {
	let mut command = command();
	command.wrap(ProcessSession);
	spawn_twice(command)
}

#[cfg(feature = "reset-sigmask")]
#[test]
fn reset_sigmask() -> Result<()> {
	let mut command = command();
	command.wrap(ResetSigmask);
	spawn_twice(command)
}
