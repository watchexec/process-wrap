use super::prelude::*;

/// Spawn the same command twice, as wrappers' setup must hold up when it runs again.
async fn spawn_twice(mut command: CommandWrap) -> Result<()> {
	for _ in 0..2 {
		let status = command.spawn()?.wait().await?;
		assert!(status.success());
	}
	Ok(())
}

fn command() -> CommandWrap {
	CommandWrap::with_new("true", |_| {})
}

#[tokio::test]
async fn nowrap() -> Result<()> {
	spawn_twice(command()).await
}

#[cfg(feature = "process-group")]
#[tokio::test]
async fn process_group() -> Result<()> {
	let mut command = command();
	command.wrap(ProcessGroup::leader());
	spawn_twice(command).await
}

#[cfg(feature = "process-session")]
#[tokio::test]
async fn process_session() -> Result<()> {
	let mut command = command();
	command.wrap(ProcessSession);
	spawn_twice(command).await
}

#[cfg(feature = "reset-sigmask")]
#[tokio::test]
async fn reset_sigmask() -> Result<()> {
	let mut command = command();
	command.wrap(ResetSigmask);
	spawn_twice(command).await
}
