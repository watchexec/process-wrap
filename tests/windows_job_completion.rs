#![cfg(all(
	windows,
	feature = "job-object",
	feature = "creation-flags",
	any(feature = "std", feature = "tokio1")
))]

use std::{
	fs,
	io::{Error, ErrorKind, Result},
	os::windows::{
		io::{AsRawHandle, FromRawHandle, OwnedHandle},
		process::CommandExt,
	},
	path::{Path, PathBuf},
	process::{Command, Stdio},
	thread,
	time::{Duration, Instant},
};

use windows::Win32::{
	Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
	System::Threading::{
		CREATE_NO_WINDOW, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
		WaitForSingleObject,
	},
};

const DEADLINE: Duration = Duration::from_secs(5);
const FIXTURE_LIFETIME: Duration = Duration::from_secs(15);
const DIRECTORY: &str = "PROCESS_WRAP_COMPLETION_FIXTURE";

fn until(mut condition: impl FnMut() -> bool) -> Result<()> {
	let start = Instant::now();
	while !condition() {
		if start.elapsed() >= DEADLINE {
			return Err(Error::new(ErrorKind::TimedOut, "fixture deadline exceeded"));
		}
		thread::sleep(Duration::from_millis(5));
	}
	Ok(())
}

// Retain exact process objects before releasing the leader. Cleanup never uses a PID scan.
struct Process(OwnedHandle);

impl Process {
	fn open(pid: u32) -> Result<Self> {
		// SAFETY: OpenProcess returns a new owned handle; no borrowed memory crosses the call.
		unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map(|handle| Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
			.map_err(Error::other)
	}

	fn exited(&self) -> bool {
		// SAFETY: this guard owns the process handle until Drop.
		match unsafe { WaitForSingleObject(HANDLE(self.0.as_raw_handle()), 0) } {
			WAIT_OBJECT_0 => true,
			WAIT_TIMEOUT => false,
			other => panic!("unexpected process wait result: {other:?}"),
		}
	}
}

impl Drop for Process {
	fn drop(&mut self) {
		// SAFETY: terminate only this fixture's retained process object, then close it once.
		unsafe {
			if !self.exited() {
				if let Err(error) = TerminateProcess(HANDLE(self.0.as_raw_handle()), 1) {
					eprintln!("fixture cleanup termination: {error}");
				}
				assert_eq!(
					WaitForSingleObject(HANDLE(self.0.as_raw_handle()), 5_000),
					WAIT_OBJECT_0
				);
			}
		}
	}
}

struct Tree {
	leader: Process,
	leaf: Process,
	directory: tempfile::TempDir,
}

impl Tree {
	fn attach(directory: tempfile::TempDir, leader: u32) -> Result<Self> {
		let leader = Process::open(leader)?;
		until(|| directory.path().join("ready").exists())?;
		let pid = fs::read_to_string(directory.path().join("pid"))?;
		let leaf = Process::open(pid.parse().map_err(Error::other)?)?;
		Ok(Self {
			leader,
			leaf,
			directory,
		})
	}

	fn exit_leader(&self) -> Result<()> {
		fs::write(self.directory.path().join("exit-leader"), b"")?;
		until(|| self.leader.exited())?;
		assert!(!self.leaf.exited());
		Ok(())
	}

	fn challenge(&self) -> Result<()> {
		// Created only AFTER wait/try_wait was exercised, not a buffered heartbeat.
		let challenge = b"new execution after the wait observation";
		fs::write(self.directory.path().join("challenge"), challenge)?;
		until(|| fs::read(self.directory.path().join("ack")).is_ok_and(|ack| ack == challenge))?;
		assert!(!self.leaf.exited());
		Ok(())
	}

	fn release(&self) -> Result<()> {
		fs::write(self.directory.path().join("release"), b"")
	}
}

fn configure(command: &mut Command, path: &Path, helper: &str) {
	command
		.args(["--exact", helper, "--ignored", "--nocapture"])
		.env(DIRECTORY, path)
		.creation_flags(CREATE_NO_WINDOW.0)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null());
}

#[test]
#[ignore = "bounded subprocess fixture"]
fn fixture_leader() -> Result<()> {
	let path = PathBuf::from(std::env::var_os(DIRECTORY).expect("fixture directory"));
	let mut command = Command::new(std::env::current_exe()?);
	configure(&mut command, &path, "fixture_leaf");
	let child = command.spawn()?;
	fs::write(path.join("pid"), child.id().to_string())?;
	let start = Instant::now();
	while !path.join("exit-leader").exists() && start.elapsed() < FIXTURE_LIFETIME {
		thread::sleep(Duration::from_millis(5));
	}
	Ok(())
}

#[test]
#[ignore = "bounded subprocess fixture"]
fn fixture_leaf() -> Result<()> {
	let path = PathBuf::from(std::env::var_os(DIRECTORY).expect("fixture directory"));
	fs::write(path.join("ready"), b"")?;
	let start = Instant::now();
	while !path.join("release").exists() && start.elapsed() < FIXTURE_LIFETIME {
		if let Ok(challenge) = fs::read(path.join("challenge")) {
			fs::write(path.join("ack"), challenge)?;
		}
		thread::sleep(Duration::from_millis(5));
	}
	Ok(())
}

#[test]
fn leader_only_wait_is_a_discriminating_control() -> Result<()> {
	let directory = tempfile::tempdir()?;
	let mut command = Command::new(std::env::current_exe()?);
	configure(&mut command, directory.path(), "fixture_leader");
	let mut child = command.spawn()?;
	let tree = Tree::attach(directory, child.id())?;
	tree.exit_leader()?;
	assert!(child.wait()?.success());
	tree.challenge()?;
	assert!(
		!tree.leaf.exited(),
		"leader-only wait must not pass the job completion check"
	);
	tree.release()?;
	until(|| tree.leaf.exited())?;
	Ok(())
}

#[cfg(feature = "std")]
mod std_tests {
	use super::*;
	use process_wrap::std::{ChildWrapper, CommandWrap, CreationFlags, JobObject};
	use std::sync::mpsc;

	fn spawn() -> Result<(Box<dyn ChildWrapper>, Tree)> {
		let directory = tempfile::tempdir()?;
		let mut command = Command::new(std::env::current_exe()?);
		configure(&mut command, directory.path(), "fixture_leader");
		let child = CommandWrap::from(command)
			.wrap(CreationFlags(CREATE_NO_WINDOW.0))
			.wrap(JobObject)
			.spawn()?;
		let tree = Tree::attach(directory, child.id())?;
		Ok((child, tree))
	}

	#[test]
	fn wait_includes_live_descendant() -> Result<()> {
		let (mut child, tree) = spawn()?;
		tree.exit_leader()?;
		let observer = tree.leaf.0.try_clone()?;
		let (sender, receiver) = mpsc::channel();
		let waiter = thread::spawn(move || {
			let result = child.wait();
			// SAFETY: observer owns this exact process handle until the thread exits.
			let exited_at_return =
				unsafe { WaitForSingleObject(HANDLE(observer.as_raw_handle()), 0) }
					== WAIT_OBJECT_0;
			sender.send((result, exited_at_return)).unwrap();
		});
		let early = receiver.recv_timeout(Duration::from_millis(100)).ok();
		tree.challenge()?;
		tree.release()?;
		let (status, exited_at_return) = match early {
			Some(result) => result,
			None => receiver.recv_timeout(DEADLINE).map_err(Error::other)?,
		};
		waiter.join().unwrap();
		assert!(status?.success());
		assert!(
			exited_at_return,
			"wait returned while retained descendant handle was unsignalled; post-observation challenge acknowledged"
		);
		Ok(())
	}

	#[test]
	fn try_wait_includes_live_descendant() -> Result<()> {
		let (mut child, tree) = spawn()?;
		tree.exit_leader()?;
		let early = child.try_wait()?;
		tree.challenge()?;
		tree.release()?;
		let mut status = None;
		until(|| {
			status = child.try_wait().unwrap();
			status.is_some()
		})?;
		assert!(tree.leaf.exited(), "descendant unsignalled at completion");
		assert!(child.try_wait()?.unwrap().success(), "cached result");
		assert!(child.wait()?.success(), "wait after try_wait");
		assert!(
			early.is_none(),
			"try_wait reported completion before new challenge was acknowledged"
		);
		Ok(())
	}

	#[test]
	fn terminate_then_wait_signals_exact_processes() -> Result<()> {
		let (mut child, tree) = spawn()?;
		child.start_kill()?;
		child.wait()?;
		assert!(tree.leader.exited());
		assert!(tree.leaf.exited());
		Ok(())
	}
}

#[cfg(feature = "tokio1")]
mod tokio_tests {
	use super::*;
	use process_wrap::tokio::{ChildWrapper, CommandWrap, CreationFlags, JobObject};
	use tokio::time::timeout;

	fn spawn(kill_on_drop: bool) -> Result<(Box<dyn ChildWrapper>, Tree)> {
		let directory = tempfile::tempdir()?;
		let mut command = Command::new(std::env::current_exe()?);
		configure(&mut command, directory.path(), "fixture_leader");
		let mut command = CommandWrap::from(tokio::process::Command::from(command));
		command.wrap(CreationFlags(CREATE_NO_WINDOW.0));
		#[cfg(feature = "kill-on-drop")]
		if kill_on_drop {
			command.wrap(process_wrap::tokio::KillOnDrop);
		}
		#[cfg(not(feature = "kill-on-drop"))]
		assert!(!kill_on_drop);
		let child = command.wrap(JobObject).spawn()?;
		let tree = Tree::attach(directory, child.id().expect("leader id"))?;
		Ok((child, tree))
	}

	#[tokio::test]
	async fn wait_includes_live_descendant() -> Result<()> {
		let (mut child, tree) = spawn(false)?;
		tree.exit_leader()?;
		let early = timeout(Duration::from_millis(100), child.wait()).await;
		tree.challenge()?;
		tree.release()?;
		let was_early = early.is_ok();
		let status = match early {
			Ok(result) => result?,
			Err(_) => timeout(DEADLINE, child.wait())
				.await
				.map_err(Error::other)??,
		};
		assert!(status.success());
		assert!(tree.leaf.exited(), "descendant unsignalled at completion");
		assert!(
			!was_early,
			"wait returned before new challenge was acknowledged"
		);
		assert!(child.wait().await?.success(), "cached wait");
		Ok(())
	}

	#[tokio::test]
	async fn try_wait_includes_live_descendant() -> Result<()> {
		let (mut child, tree) = spawn(false)?;
		tree.exit_leader()?;
		let early = child.try_wait()?;
		tree.challenge()?;
		tree.release()?;
		let start = Instant::now();
		loop {
			if child.try_wait()?.is_some() {
				break;
			}
			assert!(start.elapsed() < DEADLINE);
			tokio::time::sleep(Duration::from_millis(5)).await;
		}
		assert!(tree.leaf.exited(), "descendant unsignalled at completion");
		assert!(
			early.is_none(),
			"try_wait reported completion before new challenge was acknowledged"
		);
		assert!(child.wait().await?.success(), "wait after try_wait");
		Ok(())
	}

	#[tokio::test]
	async fn cancelled_wait_then_terminate_wait_signals_exact_processes() -> Result<()> {
		let (mut child, tree) = spawn(false)?;
		tree.exit_leader()?;
		let cancelled = timeout(Duration::from_millis(100), child.wait()).await;
		tree.challenge()?;
		child.start_kill()?;
		timeout(DEADLINE, child.wait())
			.await
			.map_err(Error::other)??;
		assert!(tree.leader.exited());
		assert!(tree.leaf.exited());
		assert!(
			cancelled.is_err(),
			"wait must remain pending until cancelled"
		);
		Ok(())
	}

	#[tokio::test]
	#[cfg(feature = "kill-on-drop")]
	async fn cancelled_wait_does_not_keep_kill_on_drop_job_open() -> Result<()> {
		let (mut child, tree) = spawn(true)?;
		tree.exit_leader()?;
		let cancelled = timeout(Duration::from_millis(100), child.wait()).await;
		drop(child);
		until(|| tree.leaf.exited())?;
		assert!(
			cancelled.is_err(),
			"wait must remain pending until cancelled"
		);
		Ok(())
	}
}
