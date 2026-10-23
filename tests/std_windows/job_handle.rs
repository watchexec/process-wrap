use std::{
	any::Any,
	io::{BufRead, BufReader, Error, ErrorKind},
	os::windows::io::{AsHandle, AsRawHandle},
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
const DESCENDANT_MARKER: &str = "PW-JOB-HANDLE-DESCENDANT:";

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
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		unsafe { TerminateProcess(self.0, 1) }.ok();
		unsafe { CloseHandle(self.0) }.ok();
	}
}

/// A process which starts another, and reports its PID.
fn tree() -> CommandWrap {
	CommandWrap::with_new(std::env::current_exe().unwrap(), |command| {
		command
			.args(["job_handle_parent", "--ignored", "--nocapture"])
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

fn job_handle(child: &dyn ChildWrapper) -> JobObjectHandle {
	(child as &dyn Any)
		.downcast_ref::<JobObjectChild>()
		.expect("JobObject is the outermost wrapper")
		.job_handle()
}

#[test]
#[ignore = "subprocess helper"]
fn job_handle_leaf() {
	sleep(Duration::from_secs(300));
}

#[test]
#[ignore = "subprocess helper"]
fn job_handle_parent() {
	// The leaf's output would go to a pipe the test closes once it has read the PID.
	let mut descendant = StdCommand::new(std::env::current_exe().unwrap())
		.args(["job_handle_leaf", "--ignored"])
		.stdout(Stdio::null())
		.spawn()
		.unwrap();
	println!("{DESCENDANT_MARKER}{}", descendant.id());
	std::io::stdout().flush().unwrap();
	descendant.wait().unwrap();
}

#[test]
fn terminates_the_job_while_another_thread_waits_on_the_child() -> Result<()> {
	let mut child = tree().wrap(JobObject).spawn()?;
	let leader = ProcessGuard::open(child.id())?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut())?)?;

	let job = job_handle(child.as_ref());
	assert_eq!(
		job.as_handle().as_raw_handle(),
		job_handle(child.as_ref()).as_handle().as_raw_handle(),
		"every job handle shares the one job handle"
	);

	let (sender, waited) = mpsc::channel();
	std::thread::spawn(move || {
		let _ = sender.send(child.wait());
	});
	job.terminate(7)?;
	let status = waited
		.recv_timeout(EXIT_TIMEOUT)
		.map_err(|_| Error::new(ErrorKind::TimedOut, "child did not exit"))??;

	assert_eq!(status.code(), Some(7));
	leader.wait_for_exit()?;
	descendant.wait_for_exit()?;
	Ok(())
}

#[test]
fn terminates_the_job_after_the_child_is_dropped() -> Result<()> {
	let mut child = tree().wrap(JobObject).spawn()?;
	let leader = ProcessGuard::open(child.id())?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut())?)?;
	let job = job_handle(child.as_ref());

	drop(child);
	assert!(
		!leader.has_exited()?,
		"dropping a std child leaves it running"
	);

	job.terminate(7)?;
	leader.wait_for_exit()?;
	descendant.wait_for_exit()?;
	Ok(())
}
