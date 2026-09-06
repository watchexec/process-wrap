#![cfg(all(
	windows,
	feature = "job-object",
	any(feature = "std", feature = "tokio1")
))]

use std::{
	io,
	os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
	path::{Path, PathBuf},
	process::{Command, Stdio},
	time::{Duration, Instant},
};
use windows::Win32::{
	Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
	System::Threading::{
		OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

struct Process(OwnedHandle);
impl Process {
	fn open(pid: u32) -> io::Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }?;
		Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
	}
	fn alive(&self) -> bool {
		match unsafe { WaitForSingleObject(HANDLE(self.0.as_raw_handle()), 0) } {
			WAIT_TIMEOUT => true,
			WAIT_OBJECT_0 => false,
			other => panic!("unexpected process wait result: {other:?}"),
		}
	}
	fn stop(&self) {
		if self.alive() {
			let _ = unsafe { TerminateProcess(HANDLE(self.0.as_raw_handle()), 99) };
		}
		assert_eq!(
			unsafe { WaitForSingleObject(HANDLE(self.0.as_raw_handle()), 10_000) },
			WAIT_OBJECT_0
		);
	}
}
impl Drop for Process {
	fn drop(&mut self) {
		self.stop();
	}
}

fn until(mut ready: impl FnMut() -> bool) {
	let deadline = Instant::now() + Duration::from_secs(20);
	while !ready() {
		assert!(Instant::now() < deadline, "fixture handshake timed out");
		std::thread::sleep(Duration::from_millis(2));
	}
}

fn fixture_command(mode: &str, directory: &Path) -> io::Result<Command> {
	let mut command = Command::new(std::env::current_exe()?);
	command
		.args(["--exact", "child_fixture", "--nocapture"])
		.env("PROCESS_WRAP_LIFETIME_MODE", mode)
		.env("PROCESS_WRAP_LIFETIME_DIRECTORY", directory)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null());
	Ok(command)
}

#[test]
fn child_fixture() -> io::Result<()> {
	let Some(mode) = std::env::var_os("PROCESS_WRAP_LIFETIME_MODE") else {
		return Ok(());
	};
	let directory = PathBuf::from(std::env::var_os("PROCESS_WRAP_LIFETIME_DIRECTORY").unwrap());
	if mode == "leaf" {
		std::fs::write(directory.join("ready"), b"ready")?;
		until(|| directory.join("release-leaf").exists());
	} else {
		let leaf = fixture_command("leaf", &directory)?.spawn()?;
		std::fs::write(directory.join("pid"), leaf.id().to_string())?;
		until(|| directory.join("release-leader").exists());
		// The surviving descendant is the condition these tests exercise.
		std::process::exit(0);
	}
	Ok(())
}

struct Fixture {
	_directory: tempfile::TempDir,
	leader: Process,
	leaf: Process,
}
impl Fixture {
	fn attach(directory: tempfile::TempDir, pid: u32) -> io::Result<Self> {
		let leader = Process::open(pid)?;
		until(|| directory.path().join("ready").exists() && directory.path().join("pid").exists());
		let leaf = Process::open(
			std::fs::read_to_string(directory.path().join("pid"))?
				.parse()
				.unwrap(),
		)?;
		std::fs::write(directory.path().join("release-leader"), b"release")?;
		until(|| !leader.alive());
		assert!(leaf.alive());
		Ok(Self {
			_directory: directory,
			leader,
			leaf,
		})
	}
	#[cfg(feature = "std")]
	fn release_leaf(&self) -> io::Result<()> {
		std::fs::write(self._directory.path().join("release-leaf"), b"release")
	}
}
impl Drop for Fixture {
	fn drop(&mut self) {
		self.leaf.stop();
		self.leader.stop();
	}
}

#[cfg(feature = "std")]
#[test]
fn std_try_wait_tracks_the_job_after_the_leader_exits() -> io::Result<()> {
	use process_wrap::std::*;
	let directory = tempfile::tempdir()?;
	let mut command = CommandWrap::from(fixture_command("leader", directory.path())?);
	let mut child = command.wrap(JobObject).spawn()?;
	let fixture = Fixture::attach(directory, child.id())?;
	assert!(
		child.try_wait()?.is_none(),
		"a live descendant keeps the job running"
	);
	fixture.release_leaf()?;
	let status = child.wait()?;
	assert!(!fixture.leaf.alive());
	assert_eq!(child.try_wait()?, Some(status));
	assert_eq!(child.wait()?, status);
	Ok(())
}

#[cfg(feature = "std")]
#[test]
fn std_wait_does_not_complete_on_a_leader_exit_notification() -> io::Result<()> {
	use process_wrap::std::*;
	let directory = tempfile::tempdir()?;
	let mut command = CommandWrap::from(fixture_command("leader", directory.path())?);
	let mut child = command.wrap(JobObject).spawn()?;
	let fixture = Fixture::attach(directory, child.id())?;
	let (send, receive) = std::sync::mpsc::channel();
	let waiter = std::thread::spawn(move || {
		let result = child.wait();
		send.send(result).unwrap();
		child
	});
	let early = receive.recv_timeout(Duration::from_millis(200));
	let alive = fixture.leaf.alive();
	fixture.release_leaf()?;
	let mut child = waiter.join().unwrap();
	assert!(
		early.is_err() && alive,
		"wait returned while a descendant was still running: {early:?}"
	);
	let status = child.wait()?;
	assert!(!fixture.leaf.alive());
	assert_eq!(child.try_wait()?, Some(status));
	Ok(())
}

#[cfg(feature = "tokio1")]
#[tokio::test]
async fn tokio_cancelled_wait_does_not_cache_the_leader_as_job_completion() -> io::Result<()> {
	use process_wrap::tokio::*;
	let directory = tempfile::tempdir()?;
	let mut command = CommandWrap::from(tokio::process::Command::from(fixture_command(
		"leader",
		directory.path(),
	)?));
	let mut child = command.wrap(JobObject).spawn()?;
	let fixture = Fixture::attach(directory, child.id().expect("leader is still running"))?;
	assert!(child.try_wait()?.is_none());
	assert!(
		tokio::time::timeout(Duration::from_millis(200), child.wait())
			.await
			.is_err()
	);
	assert!(
		child.try_wait()?.is_none(),
		"cancelled wait must not cache leader completion"
	);
	child.start_kill()?;
	let status = child.wait().await?;
	assert!(!fixture.leaf.alive());
	assert_eq!(child.wait().await?, status);
	assert_eq!(child.try_wait()?, Some(status));
	Ok(())
}

#[cfg(feature = "std")]
#[test]
fn std_kill_and_wait_finishes_descendant_termination() -> io::Result<()> {
	use process_wrap::std::*;
	let directory = tempfile::tempdir()?;
	let mut command = CommandWrap::from(fixture_command("leader", directory.path())?);
	let mut child = command.wrap(JobObject).spawn()?;
	let fixture = Fixture::attach(directory, child.id())?;
	child.start_kill()?;
	let status = child.wait()?;
	assert!(!fixture.leaf.alive());
	assert_eq!(child.wait()?, status);
	assert_eq!(child.try_wait()?, Some(status));
	Ok(())
}
