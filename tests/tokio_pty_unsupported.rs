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

use std::io;

use process_wrap::tokio::{Command, Pty, PtySize};

#[test]
fn spawning_is_explicitly_unsupported() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::default());
	assert_eq!(
		command.spawn().unwrap_err().kind(),
		io::ErrorKind::Unsupported
	);
}

#[test]
fn unsupported_takes_precedence_over_size_validation() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::new(PtySize {
		rows: 0,
		columns: 0,
		pixel_width: 0,
		pixel_height: 0,
	}));
	assert_eq!(
		command.spawn().unwrap_err().kind(),
		io::ErrorKind::Unsupported
	);
}
