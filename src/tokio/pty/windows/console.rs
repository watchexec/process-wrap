//! Dynamically owned pseudo-console handles, resizing, and off-thread close cleanup.

use std::{
	io,
	sync::{Arc, Mutex},
};

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
	state: Arc<State>,
}

#[derive(Clone, Debug)]
pub(super) struct Release {
	state: Arc<State>,
}

#[derive(Debug)]
struct State {
	handle: HPCON,
	api: &'static ConPtyApi,
	lifecycle: Mutex<Lifecycle>,
}

#[derive(Debug, Default)]
struct Lifecycle {
	released: bool,
	closed: bool,
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
			state: Arc::new(State {
				handle,
				api,
				lifecycle: Mutex::new(Lifecycle::default()),
			}),
		})
	}

	pub(super) fn handle(&self) -> HPCON {
		self.state.handle
	}

	pub(super) fn releaser(&self) -> Release {
		Release {
			state: Arc::clone(&self.state),
		}
	}

	#[cfg(test)]
	fn release(&self) -> io::Result<()> {
		self.releaser().release()
	}

	pub(super) fn resize(&self, size: PtySize) -> io::Result<()> {
		let lifecycle = self
			.state
			.lifecycle
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		if lifecycle.closed {
			return Err(io::Error::new(
				io::ErrorKind::BrokenPipe,
				"the pseudo-console is closed",
			));
		}
		// SAFETY: this is a live pseudo-console and the resolved function has the documented ABI.
		unsafe { (self.state.api.resize)(self.state.handle, coordinate(size)?) }
			.ok()
			.map_err(io::Error::other)
	}
}

impl Release {
	pub(super) fn release(&self) -> io::Result<()> {
		let mut lifecycle = self
			.state
			.lifecycle
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		if lifecycle.released || lifecycle.closed {
			return Ok(());
		}
		// SAFETY: the application owns this live pseudo-console and the resolved function has the
		// documented ABI.
		unsafe { (self.state.api.release)(self.state.handle) }
			.ok()
			.map_err(io::Error::other)?;
		lifecycle.released = true;
		Ok(())
	}
}

impl Drop for PseudoConsole {
	fn drop(&mut self) {
		let mut lifecycle = self
			.state
			.lifecycle
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		if lifecycle.closed {
			return;
		}
		lifecycle.closed = true;
		drop(lifecycle);
		schedule_close(self.state.api.close, self.state.handle);
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
	schedule_close_with(close, handle, launch_close_worker);
}

fn launch_close_worker(close: api::ClosePseudoConsole, handle: HPCON) -> io::Result<()> {
	let close_thread = std::thread::Builder::new()
		.name("process-wrap-conpty-close".into())
		.spawn(move || {
			// SAFETY: ownership of this live raw HPCON was transferred to exactly this close thread.
			unsafe { close(handle) };
		})?;
	// Dropping a JoinHandle detaches its thread. The worker exclusively owns the raw HPCON
	// transferred into its closure and will close it without blocking this caller.
	drop(close_thread);
	Ok(())
}

fn schedule_close_with(
	close: api::ClosePseudoConsole,
	handle: HPCON,
	launch: impl FnOnce(api::ClosePseudoConsole, HPCON) -> io::Result<()>,
) {
	if launch(close, handle).is_err() {
		// Calling ClosePseudoConsole here could indefinitely block a reactor or arbitrary dropping
		// thread on older Windows versions. Leaking without diagnostics is the only fallback that
		// neither blocks nor invokes application-controlled tracing on the dropping thread.
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
		time::{Duration, Instant},
	};

	#[cfg(feature = "tracing")]
	use std::{
		panic::{AssertUnwindSafe, catch_unwind},
		sync::{Condvar, mpsc},
		thread::{self, ThreadId},
	};
	#[cfg(feature = "tracing")]
	use tracing::{
		Event, Metadata, Subscriber,
		span::{Attributes, Id, Record},
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
	#[cfg(feature = "tracing")]
	static CLOSE_LAUNCH_COUNT: AtomicUsize = AtomicUsize::new(0);

	#[cfg(feature = "tracing")]
	#[derive(Clone, Copy, Debug)]
	enum DiagnosticBehavior {
		Record,
		Panic,
		Reenter,
		Block,
	}

	#[cfg(feature = "tracing")]
	#[derive(Debug, Default)]
	struct DiagnosticGate {
		events: usize,
		thread: Option<ThreadId>,
		released: bool,
	}

	#[cfg(feature = "tracing")]
	#[derive(Debug)]
	struct DiagnosticState {
		behavior: DiagnosticBehavior,
		gate: Mutex<DiagnosticGate>,
		changed: Condvar,
		reentered: AtomicBool,
	}

	#[cfg(feature = "tracing")]
	impl DiagnosticState {
		fn new(behavior: DiagnosticBehavior) -> Arc<Self> {
			Arc::new(Self {
				behavior,
				gate: Mutex::new(DiagnosticGate::default()),
				changed: Condvar::new(),
				reentered: AtomicBool::new(false),
			})
		}

		fn event_count(&self) -> usize {
			self.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.events
		}

		fn event_thread(&self) -> Option<ThreadId> {
			self.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.thread
		}

		fn release(&self) {
			let mut gate = self
				.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			gate.released = true;
			self.changed.notify_all();
		}
	}

	#[cfg(feature = "tracing")]
	#[derive(Debug)]
	struct AdversarialSubscriber {
		state: Arc<DiagnosticState>,
	}

	#[cfg(feature = "tracing")]
	impl Subscriber for AdversarialSubscriber {
		fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
			true
		}

		fn new_span(&self, _span: &Attributes<'_>) -> Id {
			Id::from_u64(1)
		}

		fn record(&self, _span: &Id, _values: &Record<'_>) {}

		fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

		fn event(&self, _event: &Event<'_>) {
			let mut gate = self
				.state
				.gate
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			gate.events += 1;
			gate.thread = Some(thread::current().id());
			self.state.changed.notify_all();
			match self.state.behavior {
				DiagnosticBehavior::Record => {}
				DiagnosticBehavior::Panic => {
					drop(gate);
					panic!("injected close diagnostic panic");
				}
				DiagnosticBehavior::Reenter => {
					drop(gate);
					self.state.reentered.store(true, Ordering::SeqCst);
					tracing::warn!("re-entered tracing from the close diagnostic subscriber");
				}
				DiagnosticBehavior::Block => {
					while !gate.released {
						gate = self.state.changed.wait(gate).unwrap();
					}
				}
			}
		}

		fn enter(&self, _span: &Id) {}

		fn exit(&self, _span: &Id) {}
	}

	#[cfg(feature = "tracing")]
	fn diagnostic_dispatch(state: Arc<DiagnosticState>) -> tracing::Dispatch {
		tracing::Dispatch::new(AdversarialSubscriber { state })
	}

	#[cfg(feature = "tracing")]
	fn fail_close_worker_start() {
		schedule_close_with(close, HPCON(42), |_, _| {
			CLOSE_LAUNCH_COUNT.fetch_add(1, Ordering::SeqCst);
			Err(io::Error::other("injected close-worker startup failure"))
		});
	}

	#[cfg(feature = "tracing")]
	fn assert_failed_launch_without_close() {
		assert_eq!(CLOSE_LAUNCH_COUNT.load(Ordering::SeqCst), 1);
		assert_eq!(CLOSE_COUNT.load(Ordering::SeqCst), 0);
	}

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
		#[cfg(feature = "tracing")]
		CLOSE_LAUNCH_COUNT.store(0, Ordering::SeqCst);
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

		let console = PseudoConsole::create_with(
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
		console.resize(PtySize::new(41, 121).unwrap()).unwrap();
		assert_eq!(
			RESIZE_SIZE.load(Ordering::SeqCst),
			pack(COORD { X: 121, Y: 41 })
		);

		drop(console);
		wait_for(|| CLOSE_COUNT.load(Ordering::SeqCst) == 1);
	}

	#[test]
	fn drop_schedules_exactly_one_close_away_from_the_caller() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		BLOCK_CLOSE.store(true, Ordering::SeqCst);
		let console = PseudoConsole {
			state: Arc::new(State {
				handle: HPCON(42),
				api: &TEST_API,
				lifecycle: Mutex::new(Lifecycle::default()),
			}),
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

	#[cfg(feature = "tracing")]
	#[test]
	fn close_worker_start_failure_does_not_invoke_a_subscriber_on_the_caller() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		let caller = thread::current().id();
		let state = DiagnosticState::new(DiagnosticBehavior::Record);
		let dispatch = diagnostic_dispatch(Arc::clone(&state));

		tracing::dispatcher::with_default(&dispatch, fail_close_worker_start);

		assert_ne!(
			state.event_thread(),
			Some(caller),
			"worker-start diagnostics must not call the subscriber on the dropping thread"
		);
		assert_eq!(state.event_count(), 0);
		assert_failed_launch_without_close();
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn close_worker_start_failure_does_not_invoke_a_panicking_subscriber() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		let state = DiagnosticState::new(DiagnosticBehavior::Panic);
		let dispatch = diagnostic_dispatch(state);

		let result = catch_unwind(AssertUnwindSafe(|| {
			tracing::dispatcher::with_default(&dispatch, fail_close_worker_start);
		}));

		assert!(
			result.is_ok(),
			"worker-start failure must not invoke a panicking subscriber"
		);
		assert_failed_launch_without_close();
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn close_worker_start_failure_does_not_reenter_tracing() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		let state = DiagnosticState::new(DiagnosticBehavior::Reenter);
		let dispatch = diagnostic_dispatch(Arc::clone(&state));

		tracing::dispatcher::with_default(&dispatch, fail_close_worker_start);

		assert!(!state.reentered.load(Ordering::SeqCst));
		assert_eq!(state.event_count(), 0);
		assert_failed_launch_without_close();
	}

	#[cfg(feature = "tracing")]
	#[test]
	fn close_worker_start_failure_does_not_block_in_a_subscriber() {
		let _serial = TEST_LOCK.lock().unwrap();
		reset_close();
		let state = DiagnosticState::new(DiagnosticBehavior::Block);
		let dispatch = diagnostic_dispatch(Arc::clone(&state));
		let (finished, finished_rx) = mpsc::channel();
		let worker = thread::spawn(move || {
			tracing::dispatcher::with_default(&dispatch, fail_close_worker_start);
			finished.send(()).unwrap();
		});

		let returned_before_watchdog = finished_rx.recv_timeout(Duration::from_secs(2)).is_ok();
		state.release();
		worker.join().unwrap();

		assert!(
			returned_before_watchdog,
			"worker-start failure blocked in an inline subscriber callback"
		);
		assert_eq!(state.event_count(), 0);
		assert_failed_launch_without_close();
	}
}
