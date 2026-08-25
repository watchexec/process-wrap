#![cfg(all(
	feature = "pty",
	not(any(
		target_os = "android",
		target_os = "dragonfly",
		target_os = "freebsd",
		target_os = "illumos",
		target_os = "linux",
		target_os = "macos",
		target_os = "netbsd",
		target_os = "openbsd",
		target_os = "solaris"
	))
))]

use std::{
	io,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
};

use process_wrap::tokio::{Command, CommandWrapper, Pty, PtySize, SpawnAttempt};

#[test]
fn spawning_is_explicitly_unsupported() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::default());
	assert_eq!(
		command.spawn().unwrap_err().kind(),
		io::ErrorKind::Unsupported
	);
}

#[derive(Debug)]
struct ObserveHook(Arc<AtomicBool>);

impl CommandWrapper for ObserveHook {
	fn pre_spawn(&mut self, _attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<()> {
		self.0.store(true, Ordering::SeqCst);
		Ok(())
	}
}

#[test]
fn unsupported_precedes_command_validation_and_hooks() {
	let called = Arc::new(AtomicBool::new(false));
	let native = tokio::process::Command::new("ignored");
	let mut command = Command::from(native);
	command
		.wrap(Pty::new(PtySize {
			rows: 0,
			columns: 0,
			pixel_width: 0,
			pixel_height: 0,
		}))
		.wrap(ObserveHook(Arc::clone(&called)));
	assert_eq!(
		command.spawn().unwrap_err().kind(),
		io::ErrorKind::Unsupported
	);
	assert!(!called.load(Ordering::SeqCst));
}
