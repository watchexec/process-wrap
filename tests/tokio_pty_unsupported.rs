#![cfg(all(feature = "pty", not(any(target_os = "linux", target_os = "macos"))))]

use std::io;

use process_wrap::tokio::{PtyCommand, PtyOptions, PtySize};

#[test]
fn spawning_is_explicitly_unsupported() {
	let mut command = PtyCommand::new("ignored");
	assert_eq!(
		command.spawn(PtyOptions::default()).unwrap_err().kind(),
		io::ErrorKind::Unsupported
	);
}

#[test]
fn unsupported_takes_precedence_over_size_validation() {
	let mut command = PtyCommand::new("ignored");
	let options = PtyOptions::new(PtySize {
		rows: 0,
		columns: 0,
		pixel_width: 0,
		pixel_height: 0,
	});
	assert_eq!(
		command.spawn(options).unwrap_err().kind(),
		io::ErrorKind::Unsupported
	);
}
