#![cfg(all(feature = "reset-sigmask", target_os = "linux"))]

use std::thread;

use nix::sys::signal::{SigSet, SigmaskHow, pthread_sigmask};

use super::prelude::*;

/// Spawn `grep` from a thread which blocks SIGUSR1, and return the blocked signals it reports.
fn child_blocked_signals(wrap: bool) -> Result<u64> {
	thread::spawn(move || {
		// A child inherits the signal mask of the thread which spawns it.
		let mut blocked = SigSet::empty();
		blocked.add(Signal::SIGUSR1);
		pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), None)?;

		let mut command = CommandWrap::with_new("grep", |command| {
			command
				.args(["SigBlk", "/proc/self/status"])
				.stdout(Stdio::piped());
		});
		if wrap {
			command.wrap(ResetSigmask);
		}
		let output = command.spawn()?.wait_with_output()?;
		assert!(output.status.success());

		let line = String::from_utf8(output.stdout).expect("status is UTF-8");
		let mask = line
			.trim()
			.strip_prefix("SigBlk:")
			.expect("grep prints the SigBlk line")
			.trim();
		Ok(u64::from_str_radix(mask, 16).expect("the mask is hexadecimal"))
	})
	.join()
	.expect("Thread panicked")
}

const SIGUSR1_BIT: u64 = 1 << (Signal::SIGUSR1 as u64 - 1);

#[test]
fn nowrap_inherits_the_mask() -> Result<()> {
	assert_ne!(child_blocked_signals(false)? & SIGUSR1_BIT, 0);
	Ok(())
}

#[test]
fn reset_sigmask() -> Result<()> {
	assert_eq!(child_blocked_signals(true)?, 0);
	Ok(())
}
