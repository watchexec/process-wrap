//! End-to-end assembly of the Windows ConPTY spawn provider.

use std::{io, sync::Arc};

use crate::{
	SpawnTransaction,
	tokio::{ProviderProduct, SpawnAttempt},
};

use super::{
	super::{ControllerSlot, PtyController, PtySize},
	api,
	attributes::AttributeList,
	console::{self, PseudoConsole, Release},
	controller,
	pipe::PipePair,
	prepare,
	spawn::{self as process, SpawnCleanup},
};

pub(in crate::tokio::pty) fn check_available() -> io::Result<()> {
	api::get().map(|_| ())
}

pub(in crate::tokio::pty) fn validate_size(size: PtySize) -> io::Result<()> {
	console::coordinate(size).map(|_| ())
}

pub(in crate::tokio::pty) fn spawn(
	attempt: &mut SpawnAttempt,
	size: PtySize,
) -> io::Result<ProviderProduct> {
	console::coordinate(size)?;
	let prepared = prepare(attempt)?;

	let input = PipePair::input()?;
	let output = PipePair::output()?;
	let console = PseudoConsole::create(size, input.server_handle(), output.server_handle())?;
	let release = console.releaser();
	let attributes = AttributeList::new(console.handle())?;
	let (input_server, input_host) = input.into_parts();
	let (output_server, output_host) = output.into_parts();
	let (input, output, resize) = controller::controller(console, input_host, output_host)?;
	let controller = Arc::new(ControllerSlot::new(PtyController::new(
		input, output, resize,
	)));

	let process::SpawnedChild { mut child, cleanup } =
		process::spawn(prepared, &attributes, input_server, output_server)?;
	child.install_controller(Arc::clone(&controller));

	Ok(ProviderProduct::new(
		Box::new(child),
		Box::new(ConPtyTransaction {
			cleanup: Some(cleanup),
			controller: Some(controller),
			release: Some(release),
		}),
	))
}

#[derive(Debug)]
struct ConPtyTransaction {
	cleanup: Option<SpawnCleanup>,
	controller: Option<Arc<ControllerSlot>>,
	release: Option<Release>,
}

impl SpawnTransaction for ConPtyTransaction {
	fn commit(&mut self) -> io::Result<()> {
		self.release
			.as_ref()
			.expect("an armed ConPTY transaction owns its release capability")
			.release()?;
		self.cleanup
			.as_mut()
			.expect("an armed ConPTY transaction owns its cleanup duplicate")
			.disarm();
		self.cleanup.take();
		self.controller.take();
		self.release.take();
		Ok(())
	}

	fn rollback(&mut self) -> io::Result<()> {
		if let Some(controller) = self.controller.take() {
			controller.rollback();
		}
		self.release.take();
		self.cleanup
			.take()
			.map_or(Ok(()), |mut cleanup| cleanup.rollback())
	}
}

#[cfg(test)]
mod tests {
	use std::{
		os::windows::io::{FromRawHandle, OwnedHandle},
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		time::Duration,
	};

	use windows::{
		Win32::{
			Foundation::HANDLE,
			System::{
				Console::{COORD, HPCON},
				Threading::CreateEventW,
			},
		},
		core::HRESULT,
	};

	use super::*;

	static RELEASES: AtomicUsize = AtomicUsize::new(0);

	unsafe extern "system" fn create(
		_size: COORD,
		_input: HANDLE,
		_output: HANDLE,
		_flags: u32,
		_pseudo_console: *mut HPCON,
	) -> HRESULT {
		HRESULT(0)
	}

	unsafe extern "system" fn resize(_pseudo_console: HPCON, _size: COORD) -> HRESULT {
		HRESULT(0)
	}

	unsafe extern "system" fn release(_pseudo_console: HPCON) -> HRESULT {
		RELEASES.fetch_add(1, Ordering::SeqCst);
		HRESULT(0)
	}

	unsafe extern "system" fn close(_pseudo_console: HPCON) {}

	static API: api::ConPtyApi = api::ConPtyApi {
		create,
		resize,
		release,
		close,
	};

	#[test]
	fn committed_transaction_releases_rollback_only_owners() -> io::Result<()> {
		RELEASES.store(0, Ordering::SeqCst);
		// SAFETY: default security, manual reset, initially unsignaled, and no name request a new event
		// handle which is transferred immediately to `OwnedHandle`.
		let event = unsafe { CreateEventW(None, true, false, None) }?;
		let expected_handle = event.0 as usize;
		// SAFETY: the successful CreateEventW result is uniquely owned here.
		let process = unsafe { OwnedHandle::from_raw_handle(event.0) };
		let (released, observe_release) = mpsc::channel();
		let cleanup = SpawnCleanup::tracked(process, released);
		let controller = Arc::new(ControllerSlot::empty_for_test());
		let controller_observer = Arc::clone(&controller);
		let (release, release_observer) = Release::tracked(&API, HPCON(42));
		let mut transaction = ConPtyTransaction {
			cleanup: Some(cleanup),
			controller: Some(controller),
			release: Some(release),
		};

		assert_eq!(Arc::strong_count(&controller_observer), 2);
		assert_eq!(release_observer.strong_count(), 1);
		transaction.commit()?;

		assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
		assert_eq!(
			observe_release
				.recv_timeout(Duration::from_secs(2))
				.unwrap(),
			expected_handle
		);
		assert_eq!(Arc::strong_count(&controller_observer), 1);
		assert_eq!(release_observer.strong_count(), 0);
		drop(transaction);
		assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
		Ok(())
	}
}
