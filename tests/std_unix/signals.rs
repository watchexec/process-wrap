use super::prelude::*;

#[test]
fn nowrap() -> Result<()> {
	let mut child = CommandWrap::with_new("yes", |command| {
		command.stdout(Stdio::null());
	})
	.spawn()?;

	child.signal(Signal::SIGCONT as _)?;
	sleep(DIE_TIME);
	assert!(child.try_wait()?.is_none(), "not exited with sigcont");

	child.signal(Signal::SIGTERM as _)?;
	sleep(DIE_TIME);
	assert!(child.try_wait()?.is_some(), "exited with sigterm");

	Ok(())
}

#[cfg(feature = "process-group")]
#[test]
fn process_group() -> Result<()> {
	let mut child = CommandWrap::with_new("yes", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?;

	child.signal(Signal::SIGCONT as _)?;
	sleep(DIE_TIME);
	assert!(child.try_wait()?.is_none(), "not exited with sigcont");

	child.signal(Signal::SIGTERM as _)?;
	sleep(DIE_TIME);
	assert!(child.try_wait()?.is_some(), "exited with sigterm");

	Ok(())
}

#[cfg(feature = "process-session")]
#[test]
fn process_session() -> Result<()> {
	let mut child = CommandWrap::with_new("yes", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessSession)
	.spawn()?;

	child.signal(Signal::SIGCONT as _)?;
	sleep(DIE_TIME);
	assert!(child.try_wait()?.is_none(), "not exited with sigcont");

	child.signal(Signal::SIGTERM as _)?;
	sleep(DIE_TIME);
	assert!(child.try_wait()?.is_some(), "exited with sigterm");

	Ok(())
}

#[test]
fn direct_wait_then_signal_and_start_kill_are_noops() -> Result<()> {
	let mut child = CommandWrap::new("true").spawn()?;
	let status = child.inner_mut().wait()?;
	child.signal(Signal::SIGCONT as _)?;
	child.start_kill()?;
	assert_eq!(child.wait()?, status);
	assert_eq!(child.try_wait()?, Some(status));
	Ok(())
}

#[cfg(feature = "process-group")]
#[test]
fn process_group_inner_wait_then_signals_are_noops() -> Result<()> {
	let mut child = CommandWrap::new("true")
		.wrap(ProcessGroup::leader())
		.spawn()?;
	let status = child.inner_mut().wait()?;
	child.signal(Signal::SIGCONT as _)?;
	child.start_kill()?;
	assert_eq!(child.wait()?, status);
	assert_eq!(child.try_wait()?, Some(status));
	Ok(())
}

#[cfg(feature = "process-session")]
#[test]
fn process_session_inner_wait_then_signals_are_noops() -> Result<()> {
	let mut child = CommandWrap::new("true").wrap(ProcessSession).spawn()?;
	let status = child.inner_mut().wait()?;
	child.signal(Signal::SIGCONT as _)?;
	child.start_kill()?;
	assert_eq!(child.wait()?, status);
	assert_eq!(child.try_wait()?, Some(status));
	Ok(())
}
