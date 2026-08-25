#![cfg(all(
	feature = "pty",
	any(
		target_os = "android",
		target_os = "dragonfly",
		target_os = "freebsd",
		target_os = "illumos",
		target_os = "linux",
		target_os = "macos",
		target_os = "netbsd",
		target_os = "openbsd",
		target_os = "solaris"
	)
))]

use std::{
	io,
	panic::{AssertUnwindSafe, catch_unwind, panic_any},
	sync::{Arc, Mutex},
	time::{Duration, Instant},
};

use nix::libc;
use process_wrap::tokio::{
	ChildWrapper, Command, CommandWrapper, ProviderProduct, Pty, PtyController, PtySize,
	SpawnAttempt, SpawnProvider,
};
use tokio::{io::AsyncReadExt, time::timeout};

fn controller(child: &mut Box<dyn ChildWrapper>) -> PtyController {
	child
		.take_pty_controller()
		.expect("a successful PTY spawn installs one controller")
}

async fn output(child: &mut dyn ChildWrapper, controller: PtyController) -> io::Result<Vec<u8>> {
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), async {
		let (status, _) = tokio::try_join!(child.wait(), output.read_to_end(&mut bytes))?;
		assert!(status.success());
		Ok::<_, io::Error>(())
	})
	.await??;
	Ok(bytes)
}

#[derive(Debug)]
struct TransparentChild(Box<dyn ChildWrapper>);

impl ChildWrapper for TransparentChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.0.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.0.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.0
	}
}

#[derive(Clone, Copy, Debug)]
struct Transparent;

impl CommandWrapper for Transparent {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_command: &Command,
	) -> io::Result<Box<dyn ChildWrapper>> {
		Ok(Box::new(TransparentChild(child)))
	}
}

#[derive(Debug)]
struct SecondTransparentChild(Box<dyn ChildWrapper>);

impl ChildWrapper for SecondTransparentChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.0.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.0.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.0
	}
}

#[derive(Clone, Copy, Debug)]
struct SecondTransparent;

impl CommandWrapper for SecondTransparent {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_command: &Command,
	) -> io::Result<Box<dyn ChildWrapper>> {
		Ok(Box::new(SecondTransparentChild(child)))
	}
}

#[derive(Debug)]
struct TerminalChild;

impl ChildWrapper for TerminalChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}
}

#[tokio::test]
async fn non_pty_children_have_no_controller() -> io::Result<()> {
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "exit 0"]);
	});
	let mut child = command.spawn()?;
	assert!(child.take_pty_controller().is_none());
	assert!(child.wait().await?.success());

	let mut child = Box::new(TerminalChild) as Box<dyn ChildWrapper>;
	assert!(child.take_pty_controller().is_none());
	Ok(())
}

#[tokio::test]
async fn controller_traversal_preserves_outer_wrappers_in_either_order() -> io::Result<()> {
	for pty_first in [false, true] {
		let mut command = Command::with_new("sh", |command| {
			command.args(["-c", "printf wrapped"]);
		});
		if pty_first {
			command
				.wrap(Pty::default())
				.wrap(Transparent)
				.wrap(SecondTransparent);
		} else {
			command
				.wrap(Transparent)
				.wrap(SecondTransparent)
				.wrap(Pty::default());
		}

		let mut child = command.spawn()?;
		assert!(child.stdin().is_none());
		assert!(child.stdout().is_none());
		assert!(child.stderr().is_none());
		let controller = controller(&mut child);
		assert!(child.take_pty_controller().is_none());
		assert_eq!(output(child.as_mut(), controller).await?, b"wrapped");
	}
	Ok(())
}

#[derive(Debug)]
struct InspectPost {
	called: Arc<Mutex<bool>>,
}

impl CommandWrapper for InspectPost {
	fn post_spawn(
		&mut self,
		attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		assert_eq!(
			attempt
				.get_portable_args()
				.expect("the PTY provider keeps portable intent")
				.len(),
			2
		);
		assert!(child.take_pty_controller().is_none());
		*self
			.called
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = true;
		Ok(())
	}
}

#[tokio::test]
async fn controller_is_committed_after_post_spawn_hooks() -> io::Result<()> {
	let called = Arc::new(Mutex::new(false));
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "printf committed"]);
	});
	command.wrap(Pty::default()).wrap(InspectPost {
		called: Arc::clone(&called),
	});

	let mut child = command.spawn()?;
	assert!(
		*called
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
	);
	let controller = controller(&mut child);
	assert_eq!(output(child.as_mut(), controller).await?, b"committed");
	Ok(())
}

#[derive(Debug)]
struct InspectWrap {
	called: Arc<Mutex<bool>>,
}

impl CommandWrapper for InspectWrap {
	fn wrap_child(
		&mut self,
		mut child: Box<dyn ChildWrapper>,
		_command: &Command,
	) -> io::Result<Box<dyn ChildWrapper>> {
		assert!(child.take_pty_controller().is_none());
		*self
			.called
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = true;
		Ok(child)
	}
}

#[tokio::test]
async fn controller_is_committed_after_child_wrapping_hooks() -> io::Result<()> {
	let called = Arc::new(Mutex::new(false));
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "printf wrapped-commit"]);
	});
	command.wrap(Pty::default()).wrap(InspectWrap {
		called: Arc::clone(&called),
	});

	let mut child = command.spawn()?;
	assert!(
		*called
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
	);
	let controller = controller(&mut child);
	assert_eq!(output(child.as_mut(), controller).await?, b"wrapped-commit");
	Ok(())
}

#[derive(Debug)]
struct SpawnPostAttempt;

impl CommandWrapper for SpawnPostAttempt {
	fn post_spawn(
		&mut self,
		attempt: &mut SpawnAttempt,
		_child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		let mut probe = attempt.native_mut().spawn()?;
		loop {
			if probe.try_wait()?.is_some() {
				return Ok(());
			}
			std::thread::yield_now();
		}
	}
}

#[tokio::test]
async fn provider_setup_is_absent_from_post_spawn_native_attempt() -> io::Result<()> {
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "exit 0"]);
	});
	command.wrap(Pty::default()).wrap(SpawnPostAttempt);

	let mut child = command.spawn()?;
	let controller = controller(&mut child);
	assert!(output(child.as_mut(), controller).await?.is_empty());
	Ok(())
}

#[tokio::test]
async fn duplicate_pty_registration_uses_the_later_size() -> io::Result<()> {
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "stty size"]);
	});
	command
		.wrap(Pty::new(PtySize::new(31, 97)?))
		.wrap(Pty::new(PtySize::new(42, 113)?));

	let mut child = command.spawn()?;
	let controller = controller(&mut child);
	assert_eq!(output(child.as_mut(), controller).await?, b"42 113\r\n");
	Ok(())
}

#[derive(Debug)]
struct OtherProvider;

impl SpawnProvider for OtherProvider {
	fn spawn(
		&self,
		_attempt: &mut SpawnAttempt,
		_command: &Command,
	) -> io::Result<ProviderProduct> {
		panic!("provider conflicts are rejected before spawning")
	}
}

impl CommandWrapper for OtherProvider {
	fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
		Some(self)
	}
}

#[test]
fn rejects_another_spawn_provider_before_callbacks() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::default()).wrap(OtherProvider);
	let error = command.spawn().unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert_eq!(error.to_string(), "multiple spawn providers are registered");
}

#[derive(Debug)]
struct MakeAttemptNative;

impl CommandWrapper for MakeAttemptNative {
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<()> {
		let _ = attempt.native_mut();
		Ok(())
	}
}

#[test]
fn rejects_opaque_base_and_attempt_state() {
	let native = tokio::process::Command::new("ignored");
	let mut command = Command::from(native);
	command.wrap(Pty::default());
	let error = command.spawn().unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert_eq!(
		error.to_string(),
		"a spawn provider cannot use a native-only command"
	);

	let mut command = Command::new("ignored");
	command.wrap(Pty::default()).wrap(MakeAttemptNative);
	let error = command.spawn().unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert_eq!(
		error.to_string(),
		"a spawn provider cannot use a native-only spawn attempt"
	);
}

#[test]
fn explicit_spawners_cannot_bypass_pty() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::default());
	let error = command
		.spawn_with(|_| panic!("explicit spawner must not run"))
		.unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

	let error = command
		.spawn_with_child(|_| panic!("explicit child spawner must not run"))
		.unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[derive(Clone, Copy, Debug)]
enum Stage {
	PostSpawn,
	WrapChild,
}

#[derive(Clone, Copy, Debug)]
enum Failure {
	Error,
	Panic,
}

#[derive(Debug)]
struct FailAfterSpawn {
	stage: Stage,
	failure: Failure,
	pid: Arc<Mutex<Option<u32>>>,
}

impl FailAfterSpawn {
	fn record(&self, child: &dyn ChildWrapper) {
		*self
			.pid
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = child.id();
	}

	fn fail<T>(&self) -> io::Result<T> {
		match self.failure {
			Failure::Error => Err(io::Error::other("fail after PTY spawn")),
			Failure::Panic => panic_any("fail after PTY spawn"),
		}
	}
}

impl CommandWrapper for FailAfterSpawn {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		if matches!(self.stage, Stage::PostSpawn) {
			self.record(child);
			self.fail()
		} else {
			Ok(())
		}
	}

	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_command: &Command,
	) -> io::Result<Box<dyn ChildWrapper>> {
		if matches!(self.stage, Stage::WrapChild) {
			self.record(child.as_ref());
			self.fail()
		} else {
			Ok(child)
		}
	}
}

fn assert_reaped(pid: u32) {
	let pid = i32::try_from(pid).unwrap();
	let mut status = 0;
	// SAFETY: status is writable, and this only queries whether the provider transaction already
	// reaped the recorded direct child.
	let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
	assert_eq!(waited, -1);
	assert_eq!(
		io::Error::last_os_error().raw_os_error(),
		Some(libc::ECHILD)
	);
}

#[derive(Debug)]
struct ReapThenFail {
	pid: Arc<Mutex<Option<u32>>>,
}

impl CommandWrapper for ReapThenFail {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		*self
			.pid
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = child.id();
		loop {
			if child.try_wait()?.is_some() {
				return Err(io::Error::other("fail after reaping PTY child"));
			}
			std::thread::yield_now();
		}
	}
}

#[tokio::test]
async fn transaction_observes_a_child_reaped_by_a_hook() {
	let pid = Arc::new(Mutex::new(None));
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "exit 0"]);
	});
	command.wrap(Pty::default()).wrap(ReapThenFail {
		pid: Arc::clone(&pid),
	});

	assert_eq!(
		command.spawn().unwrap_err().to_string(),
		"fail after reaping PTY child"
	);
	let pid = pid
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.expect("the hook records the direct child before reaping it");
	assert_reaped(pid);
}

#[derive(Debug)]
struct FailAfterDescendantStarts {
	pid_file: std::path::PathBuf,
	direct_pid: Arc<Mutex<Option<u32>>>,
	descendant_pid: Arc<Mutex<Option<i32>>>,
}

impl CommandWrapper for FailAfterDescendantStarts {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		*self
			.direct_pid
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = child.id();
		let deadline = Instant::now() + Duration::from_secs(5);
		while !self.pid_file.exists() {
			if Instant::now() >= deadline {
				return Err(io::Error::new(
					io::ErrorKind::TimedOut,
					"PTY descendant did not report its PID",
				));
			}
			std::thread::sleep(Duration::from_millis(10));
		}
		let pid = std::fs::read_to_string(&self.pid_file)?
			.trim()
			.parse()
			.map_err(io::Error::other)?;
		*self
			.descendant_pid
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pid);
		Err(io::Error::other("fail with a live PTY descendant"))
	}
}

fn assert_process_disappears(pid: i32) {
	let deadline = Instant::now() + Duration::from_secs(5);
	loop {
		// SAFETY: signal zero only queries whether a process with this PID exists.
		if unsafe { libc::kill(pid, 0) } == -1
			&& io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
		{
			return;
		}
		assert!(
			Instant::now() < deadline,
			"PTY descendant {pid} survived transaction rollback"
		);
		std::thread::sleep(Duration::from_millis(10));
	}
}

#[tokio::test]
async fn transaction_kills_the_live_pty_group_after_hook_failure() -> io::Result<()> {
	let directory = tempfile::tempdir()?;
	let pid_file = directory.path().join("descendant-pid");
	let direct_pid = Arc::new(Mutex::new(None));
	let descendant_pid = Arc::new(Mutex::new(None));
	let mut command = Command::with_new("sh", |command| {
		command
			.args([
				"-c",
				"trap '' HUP TERM; sleep 30 & printf '%s' $! > \"$1\"; wait",
				"pty-test",
			])
			.arg(&pid_file);
	});
	command
		.wrap(Pty::default())
		.wrap(FailAfterDescendantStarts {
			pid_file,
			direct_pid: Arc::clone(&direct_pid),
			descendant_pid: Arc::clone(&descendant_pid),
		});

	assert_eq!(
		command.spawn().unwrap_err().to_string(),
		"fail with a live PTY descendant"
	);
	let direct_pid = direct_pid
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.expect("the hook records the direct child");
	assert_reaped(direct_pid);
	let descendant_pid = descendant_pid
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.expect("the hook records the descendant child");
	assert_process_disappears(descendant_pid);
	Ok(())
}

#[tokio::test]
async fn transaction_reaps_children_after_hook_failures() {
	for stage in [Stage::PostSpawn, Stage::WrapChild] {
		for failure in [Failure::Error, Failure::Panic] {
			let pid = Arc::new(Mutex::new(None));
			let mut command = Command::with_new("sh", |command| {
				command.args(["-c", "trap '' HUP; sleep 30"]);
			});
			command.wrap(Pty::default()).wrap(FailAfterSpawn {
				stage,
				failure,
				pid: Arc::clone(&pid),
			});

			let result = catch_unwind(AssertUnwindSafe(|| command.spawn()));
			match failure {
				Failure::Error => assert_eq!(
					result.unwrap().unwrap_err().to_string(),
					"fail after PTY spawn"
				),
				Failure::Panic => assert_eq!(
					*result
						.expect_err("the lifecycle hook must panic")
						.downcast::<&'static str>()
						.unwrap(),
					"fail after PTY spawn"
				),
			}
			let pid = pid
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.expect("the failing hook records the direct child");
			assert_reaped(pid);
		}
	}
}

#[tokio::test]
async fn provider_and_controller_are_reusable() -> io::Result<()> {
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "printf reused"]);
	});
	command.wrap(Pty::default());

	for _ in 0..3 {
		let mut child = command.spawn()?;
		let controller = controller(&mut child);
		assert_eq!(output(child.as_mut(), controller).await?, b"reused");
	}
	Ok(())
}
