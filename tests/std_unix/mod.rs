mod prelude {
	pub use std::{
		io::{Read, Result, Write},
		os::unix::process::ExitStatusExt,
		process::Stdio,
		thread::sleep,
		time::Duration,
	};

	pub use nix::sys::signal::Signal;
	pub use process_wrap::std::*;

	pub const DIE_TIME: Duration = Duration::from_millis(100);
}

mod id_same_as_inner;
mod inner_read_stdout;
mod into_inner_write_stdin;
mod kill_and_try_wait;
mod signals;
mod try_wait_after_die;
mod try_wait_twice_after_sigterm;
mod wait_after_die;
mod wait_twice;
mod wait_twice_after_sigterm;
mod wait_with_output;
