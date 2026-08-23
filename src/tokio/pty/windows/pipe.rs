//! Split named pipes for the ConPTY-facing synchronous and host-facing overlapped endpoints.

use std::{
	ffi::OsString,
	io,
	os::windows::{
		ffi::OsStrExt,
		io::{AsRawHandle, FromRawHandle, OwnedHandle},
	},
	sync::atomic::{AtomicU64, Ordering},
};

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use windows::{
	Win32::{
		Foundation::{HANDLE, INVALID_HANDLE_VALUE},
		Storage::FileSystem::{
			FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_INBOUND,
			PIPE_ACCESS_OUTBOUND,
		},
		System::Pipes::{
			CreateNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
			PIPE_TYPE_BYTE, PIPE_WAIT,
		},
	},
	core::PCWSTR,
};

#[derive(Clone, Copy, Debug)]
enum Direction {
	Input,
	Output,
}

impl Direction {
	fn suffix(self) -> &'static str {
		match self {
			Self::Input => "in",
			Self::Output => "out",
		}
	}

	fn server_access(self) -> FILE_FLAGS_AND_ATTRIBUTES {
		let access = match self {
			Self::Input => PIPE_ACCESS_INBOUND,
			Self::Output => PIPE_ACCESS_OUTBOUND,
		};
		access | FILE_FLAG_FIRST_PIPE_INSTANCE
	}

	fn configure_client(self, options: &mut ClientOptions) {
		match self {
			Self::Input => {
				options.read(false).write(true);
			}
			Self::Output => {
				options.read(true).write(false);
			}
		}
	}
}

#[derive(Debug)]
pub(super) struct PipePair {
	server: OwnedHandle,
	host: NamedPipeClient,
}

impl PipePair {
	pub(super) fn input() -> io::Result<Self> {
		Self::create(Direction::Input)
	}

	pub(super) fn output() -> io::Result<Self> {
		Self::create(Direction::Output)
	}

	fn create(direction: Direction) -> io::Result<Self> {
		static NEXT_PIPE: AtomicU64 = AtomicU64::new(0);

		let sequence = NEXT_PIPE.fetch_add(1, Ordering::Relaxed);
		let name = OsString::from(format!(
			r"\\.\pipe\process-wrap-conpty-{}-{sequence}-{}",
			std::process::id(),
			direction.suffix()
		));
		let wide_name = name.encode_wide().chain([0]).collect::<Vec<_>>();
		let pipe_mode: NAMED_PIPE_MODE =
			PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;

		// SAFETY: wide_name is NUL-terminated and remains live for the call. Null security attributes
		// produce a non-inheritable synchronous server handle, as required by ConPTY.
		let server = unsafe {
			CreateNamedPipeW(
				PCWSTR(wide_name.as_ptr()),
				direction.server_access(),
				pipe_mode,
				1,
				0,
				0,
				0,
				None,
			)
		};
		if server == INVALID_HANDLE_VALUE {
			return Err(io::Error::last_os_error());
		}
		// SAFETY: CreateNamedPipeW returned a unique owned handle which is transferred exactly once.
		let server = unsafe { OwnedHandle::from_raw_handle(server.0) };

		let mut options = ClientOptions::new();
		direction.configure_client(&mut options);
		let host = options.open(&name)?;

		Ok(Self { server, host })
	}

	pub(super) fn server_handle(&self) -> HANDLE {
		HANDLE(self.server.as_raw_handle())
	}

	pub(super) fn into_parts(self) -> (OwnedHandle, NamedPipeClient) {
		(self.server, self.host)
	}
}
