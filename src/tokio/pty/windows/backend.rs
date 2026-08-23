//! End-to-end assembly of the Windows ConPTY backend.

use std::{
	io,
	panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
};

use super::{
	super::{ChildWrapper, PtyCommand, PtyController, PtyOptions},
	attributes::AttributeList,
	console::{self, PseudoConsole},
	controller,
	pipe::PipePair,
	prepare, spawn as process,
};

pub(in crate::tokio::pty) fn spawn(
	command: &mut PtyCommand,
	options: PtyOptions,
) -> io::Result<(Box<dyn ChildWrapper>, PtyController)> {
	console::coordinate(options.size)?;
	command.rebuild_command();
	let prepared = prepare(command)?;

	let input = PipePair::input()?;
	let output = PipePair::output()?;
	let mut console =
		PseudoConsole::create(options.size, input.server_handle(), output.server_handle())?;
	let (input_server, input_host) = input.into_parts();
	let (output_server, output_host) = output.into_parts();
	let attributes = AttributeList::new(console.handle())?;
	let mut cleanup = None;

	let spawned = catch_unwind(AssertUnwindSafe(|| {
		let cleanup = &mut cleanup;
		command.command.spawn_with_child(move |_inner| {
			let spawned = process::spawn(prepared, &attributes, input_server, output_server)?;
			*cleanup = Some(spawned.cleanup);
			Ok(Box::new(spawned.child) as Box<dyn ChildWrapper>)
		})
	}));
	command.rebuild_command();

	let mut child = match spawned {
		Ok(Ok(child)) => child,
		Ok(Err(error)) => {
			drop(cleanup.take());
			return Err(error);
		}
		Err(payload) => {
			drop(cleanup.take());
			resume_unwind(payload);
		}
	};
	if let Err(error) = console.release() {
		let _ = child.start_kill();
		drop(cleanup.take());
		return Err(error);
	}
	let (input, output, resize) = match controller::controller(console, input_host, output_host) {
		Ok(controller) => controller,
		Err(error) => {
			let _ = child.start_kill();
			drop(cleanup.take());
			return Err(error);
		}
	};
	cleanup
		.take()
		.expect("a successful ConPTY spawn must install a cleanup guard")
		.disarm();
	Ok((child, PtyController::new(input, output, resize)))
}
