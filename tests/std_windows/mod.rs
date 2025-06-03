mod prelude {
	pub use std::{
		io::{Read, Result, Write},
		process::{Command, Stdio},
		thread::sleep,
		time::Duration,
	};

	pub use process_wrap::std::*;

	pub const DIE_TIME: Duration = Duration::from_millis(1000);
}

fn init() {
	tracing_subscriber::fmt()
		.with_max_level(tracing::Level::DEBUG)
		.init();
}

mod id_same_as_inner;
mod inner_read_stdout;
mod into_inner_write_stdin;
mod kill_and_try_wait;
mod read_creation_flags;
mod try_wait_after_die;
mod wait_after_die;
mod wait_twice;
mod wait_with_output;
