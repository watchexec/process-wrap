mod prelude {
	pub use std::{io::Result, process::Stdio, time::Duration};

	pub use process_wrap::tokio::*;
	pub use tokio::{
		io::{AsyncReadExt, AsyncWriteExt},
		time::sleep,
	};

	pub const DIE_TIME: Duration = Duration::from_millis(1000);
}

#[cfg(all(feature = "creation-flags", feature = "job-object"))]
mod creation_flags_job_object;
mod id_same_as_inner;
mod inner_read_stdout;
mod into_inner_write_stdin;
#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
mod job_object_kill_on_drop;
mod kill_and_try_wait;
#[cfg(feature = "job-object")]
mod process_handle;
mod try_wait_after_die;
mod wait_after_die;
mod wait_twice;
mod wait_with_output;
#[cfg(all(feature = "creation-flags", feature = "job-object"))]
#[path = "../support/windows_thread.rs"]
mod windows_thread;
