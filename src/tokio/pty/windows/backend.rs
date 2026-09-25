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
			cleanup,
			controller,
			release,
		}),
	))
}

#[derive(Debug)]
struct ConPtyTransaction {
	cleanup: SpawnCleanup,
	controller: Arc<ControllerSlot>,
	release: Release,
}

impl SpawnTransaction for ConPtyTransaction {
	fn commit(&mut self) -> io::Result<()> {
		self.release.release()?;
		self.cleanup.disarm();
		Ok(())
	}

	fn rollback(&mut self) -> io::Result<()> {
		self.controller.rollback();
		self.cleanup.rollback()
	}
}
