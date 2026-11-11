#![cfg(feature = "kill-on-drop")]

use std::io::Error;

use windows::Win32::{
	Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

/// A process opened by ID, so its exit can be observed; terminated if a test fails before it exits.
struct ProcessGuard(HANDLE);

impl ProcessGuard {
	fn open(pid: u32) -> Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(Error::other)?;
		Ok(Self(handle))
	}

	fn has_exited(&self) -> Result<bool> {
		match unsafe { WaitForSingleObject(self.0, 0) } {
			WAIT_OBJECT_0 => Ok(true),
			WAIT_TIMEOUT => Ok(false),
			_ => Err(Error::last_os_error()),
		}
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		unsafe { TerminateProcess(self.0, 1) }.ok();
		unsafe { CloseHandle(self.0) }.ok();
	}
}

#[test]
#[ignore = "subprocess helper"]
fn kill_on_drop_leaf() {
	sleep(Duration::from_secs(300));
}

fn leaf() -> CommandWrap {
	CommandWrap::with_new(std::env::current_exe().unwrap(), |command| {
		command
			.args(["kill_on_drop_leaf", "--ignored"])
			.stdout(Stdio::null());
	})
}

#[test]
fn kill_on_drop() -> Result<()> {
	let child = leaf().wrap(KillOnDrop).spawn()?;
	let process = ProcessGuard::open(child.id())?;

	// Dropping kills the child and waits for it, so it has exited once drop returns.
	drop(child);
	assert!(process.has_exited()?, "Subprocess is still alive!");
	Ok(())
}

#[test]
fn into_inner_disarms() -> Result<()> {
	let child = leaf().wrap(KillOnDrop).spawn()?;
	let process = ProcessGuard::open(child.id())?;

	let mut inner = child.into_inner();
	assert!(
		!process.has_exited()?,
		"unwrapping the child must not kill it"
	);

	inner.kill()?;
	assert!(process.has_exited()?);
	Ok(())
}
