use std::{
	io::{BufRead, BufReader, Error, ErrorKind, Result, Write},
	process::{Child, Command},
	sync::{Arc, Mutex, mpsc},
	thread::{self, sleep},
	time::{Duration, Instant},
};

use windows::{
	Win32::{
		Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
		System::Threading::{
			CreateEventW, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, WaitForSingleObject,
		},
	},
	core::PCWSTR,
};

use super::prelude::*;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCENDANT_MARKER: &str = "PW-DESCENDANT:";

#[derive(Clone, Copy)]
enum RegistrationOrder {
	Default,
	KillOnDropFirst,
	JobObjectFirst,
}

struct ProcessGuard(Option<HANDLE>);

impl ProcessGuard {
	fn open(pid: u32) -> Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(Error::other)?;
		Ok(Self(Some(handle)))
	}

	fn handle(&self) -> HANDLE {
		self.0
			.expect("ProcessGuard is disarmed only after the process exits")
	}

	fn is_running(&self) -> Result<bool> {
		match unsafe { WaitForSingleObject(self.handle(), 0) } {
			WAIT_TIMEOUT => Ok(true),
			WAIT_OBJECT_0 => Ok(false),
			WAIT_FAILED => Err(Error::last_os_error()),
			result => Err(Error::other(format!("unexpected wait result: {result:?}"))),
		}
	}

	fn wait_for_exit(&self) -> Result<()> {
		let deadline = Instant::now() + EXIT_TIMEOUT;
		loop {
			match unsafe { WaitForSingleObject(self.handle(), 0) } {
				WAIT_OBJECT_0 => return Ok(()),
				WAIT_FAILED => return Err(Error::last_os_error()),
				WAIT_TIMEOUT if Instant::now() < deadline => sleep(Duration::from_millis(10)),
				WAIT_TIMEOUT => {
					return Err(Error::new(
						ErrorKind::TimedOut,
						"process remained alive after its job was dropped",
					));
				}
				result => return Err(Error::other(format!("unexpected wait result: {result:?}"))),
			}
		}
	}

	fn terminate(&self) -> Result<()> {
		unsafe { windows::Win32::System::Threading::TerminateProcess(self.handle(), 1) }
			.map_err(Error::other)?;
		self.wait_for_exit()
	}

	fn disarm(mut self) -> Result<()> {
		if let Some(handle) = self.0.take() {
			unsafe { CloseHandle(handle) }.map_err(Error::other)?;
		}
		Ok(())
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		if let Some(handle) = self.0.take() {
			unsafe { windows::Win32::System::Threading::TerminateProcess(handle, 1) }.ok();
			unsafe { CloseHandle(handle) }.ok();
		}
	}
}

fn descendant_pid(child: &mut dyn ChildWrapper) -> Result<u32> {
	let stdout = child.stdout().take().ok_or_else(|| {
		Error::new(
			ErrorKind::BrokenPipe,
			"descendant helper stdout was not piped",
		)
	})?;
	let (sender, receiver) = mpsc::channel();
	let reader = thread::spawn(move || {
		let mut reader = BufReader::new(stdout);
		let mut transcript = String::new();
		loop {
			let mut line = String::new();
			match reader.read_line(&mut line) {
				Ok(0) => {
					let _ = sender.send(Err(Error::other(format!(
						"descendant helper exited before reporting its process ID; output: {transcript}"
					))));
					return;
				}
				Ok(_) => {
					if let Some(pid) = line.strip_prefix(DESCENDANT_MARKER) {
						let _ = sender.send(pid.trim().parse::<u32>().map_err(Error::other));
						return;
					}
					transcript.push_str(&line);
				}
				Err(error) => {
					let _ = sender.send(Err(error));
					return;
				}
			}
		}
	});

	let result = match receiver.recv_timeout(STARTUP_TIMEOUT) {
		Ok(result) => result,
		Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::new(
			ErrorKind::TimedOut,
			"descendant helper did not report its process ID",
		)),
		Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::other(
			"descendant helper output reader exited before reporting a process ID",
		)),
	};

	if result.is_err() {
		let _ = child.start_kill();
		let _ = child.wait();
	}
	reader
		.join()
		.map_err(|_| Error::other("descendant output reader panicked"))?;
	result
}

fn command(order: RegistrationOrder) -> CommandWrap {
	let mut command = CommandWrap::with_new(
		std::env::current_exe().expect("test executable path"),
		|command| {
			command
				.args(["descendant_parent", "--ignored", "--nocapture"])
				.stdout(Stdio::piped())
				.stderr(Stdio::piped());
		},
	);
	match order {
		RegistrationOrder::Default => {
			command.wrap(JobObject);
		}
		RegistrationOrder::KillOnDropFirst => {
			command.wrap(KillOnDrop).wrap(JobObject);
		}
		RegistrationOrder::JobObjectFirst => {
			command.wrap(JobObject).wrap(KillOnDrop);
		}
	}
	command
}

#[test]
#[ignore = "subprocess helper"]
fn descendant_leaf() {
	thread::sleep(Duration::from_secs(300));
}

#[test]
#[ignore = "subprocess helper"]
fn descendant_parent() {
	let mut descendant = Command::new(std::env::current_exe().expect("test executable path"))
		.args(["descendant_leaf", "--ignored", "--nocapture"])
		.spawn()
		.expect("spawn descendant helper");
	println!("{DESCENDANT_MARKER}{}", descendant.id());
	std::io::stdout()
		.flush()
		.expect("flush descendant process ID");
	descendant.wait().expect("wait for descendant helper");
}

#[test]
fn kill_on_drop_is_opt_in_and_registration_order_independent() -> Result<()> {
	for (order, should_kill) in [
		(RegistrationOrder::Default, false),
		(RegistrationOrder::KillOnDropFirst, true),
		(RegistrationOrder::JobObjectFirst, true),
	] {
		let mut command = command(order);
		let mut child = command.spawn()?;
		let descendant_id = descendant_pid(child.as_mut())?;
		let parent = ProcessGuard::open(child.id())?;
		let descendant = ProcessGuard::open(descendant_id)?;

		drop(child);
		if should_kill {
			parent.wait_for_exit()?;
			descendant.wait_for_exit()?;
		} else {
			assert!(parent.is_running()?);
			assert!(descendant.is_running()?);
			descendant.terminate()?;
			parent.wait_for_exit()?;
		}

		parent.disarm()?;
		descendant.disarm()?;
	}
	Ok(())
}

#[derive(Debug)]
struct EventHandle(usize);

impl EventHandle {
	fn new() -> Result<Self> {
		let handle =
			unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(Error::other)?;
		Ok(Self(handle.0 as usize))
	}
}

impl Drop for EventHandle {
	fn drop(&mut self) {
		unsafe { CloseHandle(HANDLE(self.0 as _)) }.ok();
	}
}

#[derive(Debug)]
struct ObservedProcessHandle(usize);

impl ObservedProcessHandle {
	fn open(pid: u32) -> Result<Self> {
		let handle =
			unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.map_err(Error::other)?;
		Ok(Self(handle.0 as usize))
	}

	fn wait_for_exit(&self) -> Result<()> {
		let deadline = Instant::now() + EXIT_TIMEOUT;
		loop {
			match unsafe { WaitForSingleObject(HANDLE(self.0 as _), 0) } {
				WAIT_OBJECT_0 => return Ok(()),
				WAIT_FAILED => return Err(Error::last_os_error()),
				WAIT_TIMEOUT if Instant::now() < deadline => sleep(Duration::from_millis(10)),
				WAIT_TIMEOUT => {
					return Err(Error::new(
						ErrorKind::TimedOut,
						"process remained alive after job assignment failed",
					));
				}
				result => return Err(Error::other(format!("unexpected wait result: {result:?}"))),
			}
		}
	}
}

impl Drop for ObservedProcessHandle {
	fn drop(&mut self) {
		unsafe { CloseHandle(HANDLE(self.0 as _)) }.ok();
	}
}

#[derive(Clone, Debug)]
struct EventAsProcessHandle {
	event: Arc<EventHandle>,
	observed_process: Arc<Mutex<Option<ObservedProcessHandle>>>,
}

#[derive(Debug)]
struct MisreportedProcessHandleChild {
	child: Child,
	event: Arc<EventHandle>,
}

impl ChildWrapper for MisreportedProcessHandleChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
		// SAFETY: the test-owned event remains open through the spawn attempt; it is a valid handle,
		// but intentionally not a process handle, so AssignProcessToJobObject rejects it.
		Some(unsafe {
			std::os::windows::io::BorrowedHandle::borrow_raw(self.event.0 as *mut std::ffi::c_void)
		})
	}

	fn id(&self) -> u32 {
		self.child.id()
	}

	fn start_kill(&mut self) -> Result<()> {
		self.child.kill()
	}

	fn wait(&mut self) -> Result<std::process::ExitStatus> {
		self.child.wait()
	}
}

impl CommandWrapper for EventAsProcessHandle {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let child = unsafe { child.try_into_inner_child() }
			.map_err(|_| Error::other("test expected a native child"))?;
		*self
			.observed_process
			.lock()
			.expect("process observation lock") = Some(ObservedProcessHandle::open(child.id())?);
		Ok(Box::new(MisreportedProcessHandleChild {
			child,
			event: Arc::clone(&self.event),
		}))
	}
}

#[test]
fn failed_job_assignment_kills_the_suspended_process_before_its_marker_runs() -> Result<()> {
	let directory = tempfile::tempdir()?;
	let marker = directory.path().join("executed.txt");
	let mut control = Command::new("cmd.exe")
		.current_dir(directory.path())
		.args(["/D", "/S", "/C", "echo executed > executed.txt"])
		.spawn()?;
	assert!(control.wait()?.success());
	assert!(marker.exists(), "control command did not write its marker");
	std::fs::remove_file(&marker)?;

	let event = Arc::new(EventHandle::new()?);
	let observed_process = Arc::new(Mutex::new(None));
	let mut command = CommandWrap::with_new("cmd.exe", |command| {
		command.current_dir(directory.path()).args([
			"/D",
			"/S",
			"/C",
			"echo executed > executed.txt",
		]);
	});
	command
		.wrap(EventAsProcessHandle {
			event: Arc::clone(&event),
			observed_process: Arc::clone(&observed_process),
		})
		.wrap(JobObject);

	let error = command
		.spawn()
		.expect_err("an event handle cannot be assigned to a job");
	assert_eq!(error.kind(), ErrorKind::Other);
	let observed_process = observed_process
		.lock()
		.expect("process observation lock")
		.take()
		.expect("test wrapper observed the spawned process");
	observed_process.wait_for_exit()?;
	assert!(
		!marker.exists(),
		"the suspended command ran before assignment failed"
	);
	Ok(())
}
