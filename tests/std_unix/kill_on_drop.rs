#![cfg(feature = "kill-on-drop")]

use super::prelude::*;

#[test]
fn kill_on_drop() -> Result<()> {
	let child = CommandWrap::with_new("yes", |cmd| {
		cmd.stderr(Stdio::null()).stdout(Stdio::null());
	})
	.wrap(KillOnDrop)
	.spawn()?;
	let dead_pid = Pid::try_from(child.id()).expect("Failed to fit pid");
	drop(child);
	if pid_alive(dead_pid) {
		panic!("Subprocess is still alive!")
	}
	Ok(())
}

#[test]
fn into_inner_disarms() -> Result<()> {
	let child = CommandWrap::with_new("yes", |cmd| {
		cmd.stderr(Stdio::null()).stdout(Stdio::null());
	})
	.wrap(KillOnDrop)
	.spawn()?;
	let pid = Pid::try_from(child.id()).expect("Failed to fit pid");

	let mut inner = child.into_inner();
	assert!(pid_alive(pid), "unwrapping the child must not kill it");

	inner.kill()?;
	Ok(())
}
