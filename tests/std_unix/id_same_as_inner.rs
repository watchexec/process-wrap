use super::prelude::*;

#[test]
fn nowrap() -> Result<()> {
	let child = CommandWrap::with_new("echo", |command| {
		command.stdout(Stdio::null());
	})
	.spawn()?;

	assert_eq!(child.id(), child.inner().id());

	Ok(())
}

#[cfg(feature = "process-group")]
#[test]
fn process_group() -> Result<()> {
	let child = CommandWrap::with_new("echo", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?;

	assert_eq!(child.id(), child.inner().id());

	Ok(())
}

#[cfg(feature = "process-session")]
#[test]
fn process_session() -> Result<()> {
	let child = CommandWrap::with_new("echo", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessSession)
	.spawn()?;

	assert_eq!(child.id(), child.inner().id());

	Ok(())
}
