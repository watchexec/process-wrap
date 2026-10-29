#![cfg(all(feature = "propagate-thread-death", target_os = "linux"))]

use std::{error::Error, io, thread, time::Instant};

use nix::{
	sys::{
		signal::kill,
		wait::{WaitPidFlag, WaitStatus, waitpid},
	},
	unistd::Pid,
};

use super::prelude::*;

/// Waits for `pid` to exit, panicking if it is still alive after `DIE_TIME`.
fn wait_for_exit(pid: i32) -> io::Result<()> {
	let pid = Pid::from_raw(pid);
	let deadline = Instant::now() + DIE_TIME;

	loop {
		match waitpid(pid, Some(WaitPidFlag::WNOHANG))? {
			WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _) => return Ok(()),
			WaitStatus::StillAlive => {}
			other => panic!("Unexpected wait status {other:?}"),
		}

		if Instant::now() >= deadline {
			// Don't leak a `yes` process when the test fails.
			let _ = kill(pid, Signal::SIGKILL);
			let _ = waitpid(pid, None);
			panic!("Subprocess still running {DIE_TIME:?} after its parent thread exited");
		}

		sleep(Duration::from_millis(5));
	}
}

#[test]
fn propagate_thread_death() -> std::result::Result<(), Box<dyn Error>> {
	let handle = thread::spawn(|| {
		let child = CommandWrap::with_new("yes", |cmd| {
			cmd.stderr(Stdio::null()).stdout(Stdio::null());
		})
		.wrap(PropagateThreadDeath::default())
		.spawn()?;
		Ok::<_, io::Error>(child.id())
	});
	let dead_pid = handle
		.join()
		.expect("Thread panicked")
		.expect("Thread failed to spawn");
	let dead_pid = i32::try_from(dead_pid).expect("Failed to fit pid into i32");
	wait_for_exit(dead_pid)?;
	Ok(())
}
