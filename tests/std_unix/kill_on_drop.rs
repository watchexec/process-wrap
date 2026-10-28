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
