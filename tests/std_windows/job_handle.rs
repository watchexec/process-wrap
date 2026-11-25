use std::{
	any::Any,
	io::{BufRead, BufReader, Error, ErrorKind},
	os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
	process::Command as StdCommand,
	sync::mpsc,
	time::Instant,
};

#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
use std::{env, fs, path::Path};

use windows::Win32::{
	Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCENDANT_MARKER: &str = "PW-JOB-HANDLE-DESCENDANT:";
#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
const EXECUTION_MARKER_ENV: &str = "PW_JOB_OBJECT_EXECUTION_MARKER";

/// A process opened by ID, so its exit can be observed; terminated if a test fails before it exits.
#[derive(Debug)]
struct ProcessGuard(OwnedHandle);

impl ProcessGuard {
	fn open(pid: u32) -> Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(Error::other)?;
		// SAFETY: OpenProcess returned a new owned handle on success.
		Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
	}

	fn has_exited(&self) -> Result<bool> {
		match unsafe { WaitForSingleObject(self.handle(), 0) } {
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
		unsafe { TerminateProcess(self.handle(), 1) }.map_err(Error::other)
	}

	fn borrowed_handle(&self) -> BorrowedHandle<'_> {
		self.0.as_handle()
	}

	fn handle(&self) -> HANDLE {
		HANDLE(self.0.as_raw_handle())
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		unsafe { TerminateProcess(self.handle(), 1) }.ok();
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
		.clone()
}

#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
fn marker_writer(marker: &Path) -> CommandWrap {
	CommandWrap::with_new(std::env::current_exe().unwrap(), |command| {
		command
			.args(["job_handle_marker_writer", "--ignored", "--nocapture"])
			.env(EXECUTION_MARKER_ENV, marker)
			.stdout(Stdio::null())
			.stderr(Stdio::null());
	})
}

#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
fn wait_for_marker(marker: &Path) -> Result<()> {
	let deadline = Instant::now() + EXIT_TIMEOUT;
	loop {
		match fs::read(marker) {
			Ok(content) if content == b"started" => return Ok(()),
			Ok(_) => {}
			Err(error) if error.kind() == ErrorKind::NotFound => {}
			Err(error) => return Err(error),
		}
		if Instant::now() >= deadline {
			return Err(Error::new(
				ErrorKind::TimedOut,
				"child did not write the complete execution marker",
			));
		}
		sleep(Duration::from_millis(10));
	}
}

#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
#[derive(Debug)]
struct RestrictedProcessHandleChild {
	inner: Box<dyn ChildWrapper>,
	process_guard: ProcessGuard,
}

#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
impl ChildWrapper for RestrictedProcessHandleChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}

	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		Some(self.process_guard.borrowed_handle())
	}
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
#[ignore = "subprocess helper"]
#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
fn job_handle_marker_writer() {
	let marker = env::var_os(EXECUTION_MARKER_ENV).expect("marker path is supplied by the parent");
	fs::write(marker, b"started").unwrap();
	sleep(Duration::from_secs(300));
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

#[test]
fn default_job_object_does_not_kill_processes_when_its_handle_closes() -> Result<()> {
	let mut child = tree().wrap(JobObject).spawn()?;
	let leader = ProcessGuard::open(child.id())?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut())?)?;

	drop(child);
	assert!(
		!leader.has_exited()?,
		"the default Job Object must not enable kill-on-close"
	);
	assert!(
		!descendant.has_exited()?,
		"the default Job Object must leave descendants running"
	);

	descendant.terminate()?;
	descendant.wait_for_exit()?;
	leader.wait_for_exit()?;
	Ok(())
}

#[test]
#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
fn kill_on_drop_closes_the_job_and_terminates_its_descendants() -> Result<()> {
	let mut command = tree();
	command.wrap(KillOnDrop).wrap(JobObject);
	let mut child = command.spawn()?;
	let leader = ProcessGuard::open(child.id())?;
	let descendant = ProcessGuard::open(descendant_pid(child.as_mut())?)?;
	let job = job_handle(child.as_ref());

	// The inner KillOnDrop wrapper stops the leader. The cloned handle keeps the Job Object open,
	// so the descendant must remain alive until the opt-in kill-on-close handle is released.
	drop(child);
	leader.wait_for_exit()?;
	assert!(
		!descendant.has_exited()?,
		"the cloned job handle keeps the descendant alive"
	);

	drop(job);
	descendant.wait_for_exit()?;
	Ok(())
}

#[test]
#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
fn child_does_not_execute_until_job_assignment_succeeds() -> Result<()> {
	let directory = tempfile::tempdir()?;
	let marker = directory.path().join("started");
	let mut command = marker_writer(&marker);
	command.wrap(JobObject);

	let mut child = command.spawn_with(|command| {
		let child = command.spawn()?;
		assert!(super::windows_thread::process_has_suspended_thread(
			child.id()
		)?);
		assert!(
			!marker.exists(),
			"child code ran before Job Object assignment"
		);
		Ok(child)
	})?;
	let leader = ProcessGuard::open(child.id())?;
	wait_for_marker(&marker)?;

	child.start_kill()?;
	let _ = child.wait()?;
	leader.wait_for_exit()?;
	Ok(())
}

#[test]
#[cfg(all(feature = "kill-on-drop", feature = "creation-flags"))]
fn job_setup_failure_kills_the_suspended_child_without_running_it() -> Result<()> {
	let directory = tempfile::tempdir()?;
	let marker = directory.path().join("started");
	let mut command = marker_writer(&marker);
	command.wrap(JobObject);
	let mut process_guard = None;
	let result = command.spawn_with_child(|command| {
		let child = command.spawn()?;
		process_guard = Some(ProcessGuard::open(child.id())?);
		let restricted_process_handle = ProcessGuard::open(child.id())?;
		assert!(super::windows_thread::process_has_suspended_thread(
			child.id()
		)?);
		assert!(
			!marker.exists(),
			"child code ran before Job Object assignment"
		);
		Ok(Box::new(RestrictedProcessHandleChild {
			inner: Box::new(child),
			process_guard: restricted_process_handle,
		}))
	});

	assert!(
		result.is_err(),
		"an access-limited process handle cannot be assigned"
	);
	process_guard
		.ok_or_else(|| Error::other("the spawner did not retain the child process handle"))?
		.wait_for_exit()?;
	assert!(
		!marker.exists(),
		"failed assignment must not resume the child"
	);
	Ok(())
}
