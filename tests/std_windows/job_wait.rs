use std::{
	io::{BufRead, BufReader, Error, ErrorKind},
	process::Command as StdCommand,
	sync::mpsc,
	time::Instant,
};

use windows::Win32::{
	Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCENDANT_MARKER: &str = "PW-JOB-WAIT-DESCENDANT:";

/// A process opened by ID, so its exit can be observed and forced; terminated on drop.
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

	fn wait_for_exit(&self) -> Result<()> {
		let deadline = Instant::now() + EXIT_TIMEOUT;
		while !self.has_exited()? {
			if Instant::now() >= deadline {
				return Err(Error::new(ErrorKind::TimedOut, "process did not exit"));
			}
			sleep(Duration::from_millis(10));
		}
		Ok(())
	}

	fn terminate(&self) -> Result<()> {
		unsafe { TerminateProcess(self.0, 1) }.map_err(Error::other)
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		unsafe { TerminateProcess(self.0, 1) }.ok();
		unsafe { CloseHandle(self.0) }.ok();
	}
}

/// A process which starts another, reports its PID, and exits while it keeps running.
fn tree() -> CommandWrap {
	CommandWrap::with_new(std::env::current_exe().unwrap(), |command| {
		command
			.args(["job_wait_parent", "--ignored", "--nocapture"])
			.stdout(Stdio::piped());
	})
}

fn descendant_pid(child: &mut dyn ChildWrapper) -> Result<u32> {
	let stdout = child.stdout().take().expect("the helper's stdout is piped");
	for line in BufReader::new(stdout).lines() {
		if let Some(pid) = line?.strip_prefix(DESCENDANT_MARKER) {
			return pid.trim().parse().map_err(Error::other);
		}
	}
	Err(Error::other(
		"helper exited before reporting its descendant",
	))
}

/// Spawn the tree in a job, and return once its leader has exited.
fn orphaned_descendant() -> Result<(Box<dyn ChildWrapper>, ProcessGuard)> {
	let mut child = tree().wrap(JobObject).spawn()?;
	let leader = ProcessGuard::open(child.id())?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut())?)?;
	leader.wait_for_exit()?;
	assert!(!descendant.has_exited()?, "descendant outlives the leader");
	Ok((child, descendant))
}

#[test]
#[ignore = "subprocess helper"]
fn job_wait_leaf() {
	sleep(Duration::from_secs(300));
}

#[test]
#[ignore = "subprocess helper"]
fn job_wait_parent() {
	let descendant = StdCommand::new(std::env::current_exe().unwrap())
		.args(["job_wait_leaf", "--ignored"])
		.stdout(Stdio::null())
		.spawn()
		.unwrap();
	println!("{DESCENDANT_MARKER}{}", descendant.id());
	std::io::stdout().flush().unwrap();
	// Exit before the harness writes more, as the test stops reading at the marker.
	std::process::exit(0);
}

#[test]
fn wait_returns_once_the_descendant_exits() -> Result<()> {
	let (mut child, descendant) = orphaned_descendant()?;

	let (sender, waited) = mpsc::channel();
	std::thread::spawn(move || {
		let _ = sender.send(child.wait());
	});
	assert!(
		waited.recv_timeout(DIE_TIME).is_err(),
		"wait returned while a descendant was running"
	);

	descendant.terminate()?;
	let status = waited
		.recv_timeout(EXIT_TIMEOUT)
		.map_err(|_| Error::new(ErrorKind::TimedOut, "wait did not return"))??;
	assert!(status.success(), "wait reports the leader's status");
	Ok(())
}

#[test]
fn try_wait_is_none_until_the_descendant_exits() -> Result<()> {
	let (mut child, descendant) = orphaned_descendant()?;

	for _ in 0..5 {
		assert!(
			child.try_wait()?.is_none(),
			"try_wait returned while a descendant was running"
		);
		sleep(Duration::from_millis(50));
	}

	descendant.terminate()?;
	descendant.wait_for_exit()?;
	let deadline = Instant::now() + EXIT_TIMEOUT;
	let status = loop {
		if let Some(status) = child.try_wait()? {
			break status;
		}
		if Instant::now() >= deadline {
			return Err(Error::new(ErrorKind::TimedOut, "try_wait stayed None"));
		}
		sleep(Duration::from_millis(10));
	};
	assert!(status.success(), "try_wait reports the leader's status");

	// Polling may have drained the port; a later wait must not block on a notification that's gone.
	let (sender, waited) = mpsc::channel();
	std::thread::spawn(move || {
		let _ = sender.send(child.wait());
	});
	assert!(
		waited
			.recv_timeout(EXIT_TIMEOUT)
			.map_err(|_| Error::new(ErrorKind::TimedOut, "wait after try_wait blocked"))??
			.success()
	);
	Ok(())
}
