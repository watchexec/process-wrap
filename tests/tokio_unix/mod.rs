mod prelude {
	pub use std::{io::Result, os::unix::process::ExitStatusExt, process::Stdio, time::Duration};

	pub use nix::sys::signal::Signal;
	pub use process_wrap::tokio::*;
	#[cfg(all(
		target_os = "linux",
		feature = "kill-on-drop",
		any(feature = "process-group", feature = "process-session")
	))]
	pub use tokio::io::{AsyncBufReadExt, BufReader};
	pub use tokio::{
		io::{AsyncReadExt, AsyncWriteExt},
		time::sleep,
	};

	pub const DIE_TIME: Duration = Duration::from_millis(100);

	#[cfg(all(
		target_os = "linux",
		feature = "kill-on-drop",
		any(feature = "process-group", feature = "process-session")
	))]
	#[track_caller]
	pub fn pid_alive(pid: i32) -> bool {
		#[inline]
		#[track_caller]
		fn inner(pid: i32) -> std::result::Result<bool, remoteprocess::Error> {
			Ok(!remoteprocess::Process::new(pid)?.threads()?.is_empty())
		}

		inner(pid).unwrap_or(false)
	}
}

mod id_same_as_inner;
mod inner_read_stdout;
mod into_inner_write_stdin;
mod kill_and_try_wait;
#[cfg(all(
	target_os = "linux",
	feature = "kill-on-drop",
	any(feature = "process-group", feature = "process-session")
))]
mod multiproc_linux;
mod signals;
mod try_wait_after_die;
mod try_wait_twice_after_sigterm;
mod wait_after_die;
mod wait_twice;
mod wait_twice_after_sigterm;
mod wait_with_output;
