use std::{
	io::{Error, Result},
	os::unix::process::CommandExt,
	process::Command,
};

use nix::unistd::{setsid, Pid};
use tracing::instrument;

use super::{StdCommandWrap, StdCommandWrapper};

#[derive(Debug, Clone)]
pub struct ProcessSession;

impl StdCommandWrapper for ProcessSession {
	#[instrument(level = "debug", skip(self))]
	fn pre_spawn(&mut self, command: &mut Command, _core: &StdCommandWrap) -> Result<()> {
		unsafe {
			command.pre_exec(move || setsid().map_err(Error::from).map(|_| ()));
		}

		Ok(())
	}

	#[instrument(level = "debug", skip(self))]
	fn wrap_child(
		&mut self,
		inner: Box<dyn super::core::StdChildWrapper>,
		_core: &StdCommandWrap,
	) -> Result<Box<dyn super::core::StdChildWrapper>> {
		let pgid = Pid::from_raw(i32::try_from(inner.id()).expect("Command PID > i32::MAX"));

		Ok(Box::new(super::ProcessGroupChild::new(inner, pgid)))
	}
}
