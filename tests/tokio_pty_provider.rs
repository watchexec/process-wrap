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
	time::Duration,
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
			command.wrap(Pty::default()).wrap(Transparent);
		} else {
			command.wrap(Transparent).wrap(Pty::default());
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
