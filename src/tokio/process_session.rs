use std::io::{Error, Result};

use nix::unistd::{setsid, Pid};
use tokio::process::Command;

use super::{TokioCommandWrap, TokioCommandWrapper};

#[derive(Debug, Clone)]
pub struct ProcessSession;

impl TokioCommandWrapper for ProcessSession {
	fn pre_spawn(&mut self, command: &mut Command, _core: &TokioCommandWrap) -> Result<()> {
		unsafe {
			command.pre_exec(move || setsid().map_err(Error::from).map(|_| ()));
		}

		Ok(())
	}

	fn wrap_child(
		&mut self,
		inner: Box<dyn super::core::TokioChildWrapper>,
		_core: &TokioCommandWrap,
	) -> Result<Box<dyn super::core::TokioChildWrapper>> {
		let pgid = Pid::from_raw(
			i32::try_from(
				inner
					.id()
					.expect("Command was reaped before we could read its PID"),
			)
			.expect("Command PID > i32::MAX"),
		);

		Ok(Box::new(super::ProcessGroupChild::new(inner, pgid)))
	}
}
