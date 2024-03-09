use std::io::{Error, Result};

use nix::unistd::{setsid, Pid};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ProcessSession;

impl super::core::TokioCommandWrapper for ProcessSession {
	fn pre_spawn(&mut self, command: &mut Command) -> Result<()> {
		unsafe {
			command.pre_exec(move || setsid().map_err(Error::from).map(|_| ()));
		}

		Ok(())
	}

	fn wrap_child(
		&mut self,
		inner: Box<dyn super::core::TokioChildWrapper>,
	) -> Result<Box<dyn super::core::TokioChildWrapper>> {
		let pgid = Pid::from_raw(
			i32::try_from(
				inner
					.id()
					.expect("Command was reaped before we could read its PID"),
			)
			.expect("Command PID > i32::MAX"),
		);

		Ok(Box::new(super::ProcessGroupChild { inner, pgid }))
	}
}
