#![cfg(feature = "kill-on-drop")]

use super::prelude::*;

#[test]
fn kill_on_drop() -> Result<()> {
	let child = CommandWrap::with_new("yes", |cmd| {
		cmd.stderr(Stdio::null()).stdout(Stdio::null());
	})
	.wrap(KillOnDrop)
	.spawn()?;
	let dead_pid = nix::unistd::Pid::from_raw(child.id() as _);
	drop(child);
	// Dropping kills and reaps the child, so its PID no longer exists.
	assert_eq!(
		nix::sys::signal::kill(dead_pid, None),
		Err(nix::errno::Errno::ESRCH),
		"Subprocess is still alive!"
	);
	Ok(())
}

#[test]
fn into_inner_disarms() -> Result<()> {
	let child = CommandWrap::with_new("yes", |cmd| {
		cmd.stderr(Stdio::null()).stdout(Stdio::null());
	})
	.wrap(KillOnDrop)
	.spawn()?;
	let mut inner = child.into_inner();
	assert!(
		inner.try_wait()?.is_none(),
		"unwrapping the child must not kill it"
	);

	inner.kill()?;
	Ok(())
}
