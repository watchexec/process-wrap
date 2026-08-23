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
	released: bool,
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
		Ok(Self {
			handle,
			api,
			released: false,
		})
	}

	pub(super) fn handle(&self) -> HPCON {
		self.handle
	}

	pub(super) fn release(&mut self) -> io::Result<()> {
		if self.released {
			return Ok(());
		}
		// SAFETY: self owns a live pseudo-console and the resolved function has the documented ABI.
		unsafe { (self.api.release)(self.handle) }
			.ok()
			.map_err(io::Error::other)?;
		self.released = true;
		Ok(())
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

#[cfg(test)]
mod tests {
	use std::{
		sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
		time::{Duration, Instant},
	};

	use windows::core::HRESULT;

	use super::*;

	static TEST_LOCK: Mutex<()> = Mutex::new(());
	static CREATE_SIZE: AtomicU32 = AtomicU32::new(0);
	static CREATE_FLAGS: AtomicU32 = AtomicU32::new(u32::MAX);
	static RESIZE_SIZE: AtomicU32 = AtomicU32::new(0);
	static RELEASE_COUNT: AtomicUsize = AtomicUsize::new(0);
	static BLOCK_CLOSE: AtomicBool = AtomicBool::new(false);
	static CLOSE_STARTED: AtomicBool = AtomicBool::new(false);
	static CLOSE_COUNT: AtomicUsize = AtomicUsize::new(0);

	unsafe extern "system" fn create(
		size: COORD,
		_input: HANDLE,
		_output: HANDLE,
		flags: u32,
		pseudo_console: *mut HPCON,
	) -> HRESULT {
		CREATE_SIZE.store(pack(size), Ordering::SeqCst);
		CREATE_FLAGS.store(flags, Ordering::SeqCst);
		// SAFETY: the test caller passes writable storage for the output handle.
		unsafe { *pseudo_console = HPCON(42) };
		HRESULT(0)
	}

	unsafe extern "system" fn resize(_pseudo_console: HPCON, size: COORD) -> HRESULT {
		RESIZE_SIZE.store(pack(size), Ordering::SeqCst);
		HRESULT(0)
	}

	unsafe extern "system" fn release(_pseudo_console: HPCON) -> HRESULT {
		RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
		HRESULT(0)
	}

	unsafe extern "system" fn close(_pseudo_console: HPCON) {
		CLOSE_STARTED.store(true, Ordering::SeqCst);
		for _ in 0..1_000 {
			if !BLOCK_CLOSE.load(Ordering::SeqCst) {
				break;
			}
			std::thread::sleep(Duration::from_millis(1));
		}
		CLOSE_COUNT.fetch_add(1, Ordering::SeqCst);
	}

	static TEST_API: ConPtyApi = ConPtyApi {
		create,
		resize,
		release,
		close,
	};

	fn pack(size: COORD) -> u32 {
		u32::from(size.X as u16) | (u32::from(size.Y as u16) << 16)
	}

	fn wait_for(predicate: impl Fn() -> bool) {
		for _ in 0..1_000 {
			if predicate() {
				return;
			}
			std::thread::sleep(Duration::from_millis(1));
		}
		panic!("timed out waiting for mock ConPTY operation");
	}

	fn reset_close() {
		RELEASE_COUNT.store(0, Ordering::SeqCst);
		BLOCK_CLOSE.store(false, Ordering::SeqCst);
		CLOSE_STARTED.store(false, Ordering::SeqCst);
		CLOSE_COUNT.store(0, Ordering::SeqCst);
	}

	#[test]
	fn converts_character_dimensions_and_rejects_windows_overflow() {
		assert_eq!(
			coordinate(PtySize::new(24, 80).unwrap()).unwrap(),
			COORD { X: 80, Y: 24 }
		);
		let columns = coordinate(PtySize {
			rows: 1,
			columns: 32_768,
			pixel_width: 0,
			pixel_height: 0,
		})
		.unwrap_err();
		assert_eq!(columns.kind(), io::ErrorKind::InvalidInput);
		assert_eq!(
			columns.to_string(),
			"PTY columns must not exceed 32767 on Windows"
		);
		let rows = coordinate(PtySize {
			rows: 32_768,
			columns: 1,
			pixel_width: 0,
			pixel_height: 0,
		})
		.unwrap_err();
		assert_eq!(rows.kind(), io::ErrorKind::InvalidInput);
		assert_eq!(
			rows.to_string(),
			"PTY rows must not exceed 32767 on Windows"
		);
	}

	#[test]
	fn creates_and_resizes_through_the_resolved_capabilities() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		CREATE_SIZE.store(0, Ordering::SeqCst);
		CREATE_FLAGS.store(u32::MAX, Ordering::SeqCst);
		RESIZE_SIZE.store(0, Ordering::SeqCst);

		let mut console = PseudoConsole::create_with(
			&TEST_API,
			PtySize::new(25, 81).unwrap(),
			HANDLE(1usize as _),
			HANDLE(2usize as _),
		)
		.unwrap();
		assert_eq!(console.handle(), HPCON(42));
		assert_eq!(
			CREATE_SIZE.load(Ordering::SeqCst),
			pack(COORD { X: 81, Y: 25 })
		);
		assert_eq!(CREATE_FLAGS.load(Ordering::SeqCst), 0);
		console.resize(PtySize::new(40, 120).unwrap()).unwrap();
		assert_eq!(
			RESIZE_SIZE.load(Ordering::SeqCst),
			pack(COORD { X: 120, Y: 40 })
		);
		console.release().unwrap();
		console.release().unwrap();
		assert_eq!(RELEASE_COUNT.load(Ordering::SeqCst), 1);

		drop(console);
		wait_for(|| CLOSE_COUNT.load(Ordering::SeqCst) == 1);
	}

	#[test]
	fn drop_schedules_exactly_one_close_away_from_the_caller() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		BLOCK_CLOSE.store(true, Ordering::SeqCst);
		let console = PseudoConsole {
			handle: HPCON(42),
			api: &TEST_API,
			released: false,
		};

		let started = Instant::now();
		drop(console);
		assert!(started.elapsed() < Duration::from_millis(250));
		wait_for(|| CLOSE_STARTED.load(Ordering::SeqCst));
		assert_eq!(CLOSE_COUNT.load(Ordering::SeqCst), 0);

		BLOCK_CLOSE.store(false, Ordering::SeqCst);
		wait_for(|| CLOSE_COUNT.load(Ordering::SeqCst) == 1);
		std::thread::sleep(Duration::from_millis(10));
		assert_eq!(CLOSE_COUNT.load(Ordering::SeqCst), 1);
	}
}
