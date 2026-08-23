//! Dynamically owned pseudo-console handles, resizing, and off-thread close cleanup.

use std::{
	io,
	sync::{Mutex, OnceLock},
	thread::JoinHandle,
};

#[cfg(feature = "tracing")]
use tracing::warn;
use windows::Win32::{
	Foundation::HANDLE,
	System::Console::{COORD, HPCON},
};

use super::{
	super::PtySize,
	api::{self, ConPtyApi},
};

#[derive(Debug)]
pub(super) struct PseudoConsole {
	handle: HPCON,
	api: &'static ConPtyApi,
}

impl PseudoConsole {
	pub(super) fn create(size: PtySize, input: HANDLE, output: HANDLE) -> io::Result<Self> {
		Self::create_with(api::get()?, size, input, output)
	}

	fn create_with(
		api: &'static ConPtyApi,
		size: PtySize,
		input: HANDLE,
		output: HANDLE,
	) -> io::Result<Self> {
		let mut handle = HPCON::default();
		// SAFETY: input and output are live synchronous pipe handles, handle points to initialized
		// writable storage, and the resolved function has the documented ConPTY ABI.
		unsafe { (api.create)(coordinate(size)?, input, output, 0, &mut handle) }
			.ok()
			.map_err(io::Error::other)?;
		if handle.is_invalid() {
			return Err(io::Error::other(
				"CreatePseudoConsole returned an invalid handle",
			));
		}
		Ok(Self { handle, api })
	}

	pub(super) fn handle(&self) -> HPCON {
		self.handle
	}

	pub(super) fn resize(&self, size: PtySize) -> io::Result<()> {
		// SAFETY: self owns a live pseudo-console and the resolved function has the documented ABI.
		unsafe { (self.api.resize)(self.handle, coordinate(size)?) }
			.ok()
			.map_err(io::Error::other)
	}
}

impl Drop for PseudoConsole {
	fn drop(&mut self) {
		schedule_close(self.api.close, self.handle);
	}
}

pub(super) fn coordinate(size: PtySize) -> io::Result<COORD> {
	size.validate()?;
	let columns = i16::try_from(size.columns).map_err(|_| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"PTY columns must not exceed 32767 on Windows",
		)
	})?;
	let rows = i16::try_from(size.rows).map_err(|_| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"PTY rows must not exceed 32767 on Windows",
		)
	})?;
	Ok(COORD {
		X: columns,
		Y: rows,
	})
}

fn schedule_close(close: api::ClosePseudoConsole, handle: HPCON) {
	static CLOSE_THREADS: OnceLock<Mutex<Vec<JoinHandle<()>>>> = OnceLock::new();
	let threads = CLOSE_THREADS.get_or_init(|| Mutex::new(Vec::new()));
	let mut threads = threads.lock().unwrap_or_else(|poison| poison.into_inner());
	let mut index = 0;
	while index < threads.len() {
		if threads[index].is_finished() {
			let finished = threads.swap_remove(index);
			let _ = finished.join();
		} else {
			index += 1;
		}
	}

	let close_thread = std::thread::Builder::new()
		.name("process-wrap-conpty-close".into())
		.spawn(move || {
			// SAFETY: ownership of this live raw HPCON was transferred to exactly this close thread.
			unsafe { close(handle) };
		});
	match close_thread {
		Ok(close_thread) => threads.push(close_thread),
		Err(error) => {
			// Calling ClosePseudoConsole here could indefinitely block a reactor or arbitrary dropping
			// thread on older Windows versions. Leaking is the only safe fallback after thread creation
			// itself fails.
			#[cfg(feature = "tracing")]
			warn!(
				?error,
				"failed to start ConPTY close thread; leaking the pseudo-console handle"
			);
			#[cfg(not(feature = "tracing"))]
			let _ = error;
		}
	}
}
