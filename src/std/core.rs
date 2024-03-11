use std::{
	io::Result,
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output},
};

#[cfg(unix)]
use nix::{
	sys::signal::{kill, Signal},
	unistd::Pid,
};

crate::generic_wrap::Wrap!(StdCommandWrap, Command, StdCommandWrapper, StdChildWrapper);

pub trait StdCommandWrapper: std::fmt::Debug {
	// process-wrap guarantees that `other` will be of the same type as `self`
	// note that other crates that may use this trait should guarantee this, but
	// that cannot be enforced by the type system, so you should still panic if
	// downcasting fails, instead of potentially causing UB
	fn extend(&mut self, _other: Box<dyn StdCommandWrapper>) {}

	fn pre_spawn(&mut self, _command: &mut Command, _core: &StdCommandWrap) -> Result<()> {
		Ok(())
	}

	fn post_spawn(&mut self, _child: &mut Child, _core: &StdCommandWrap) -> Result<()> {
		Ok(())
	}

	fn wrap_child(
		&mut self,
		child: Box<dyn StdChildWrapper>,
		_core: &StdCommandWrap,
	) -> Result<Box<dyn StdChildWrapper>> {
		Ok(child)
	}
}

pub trait StdChildWrapper: std::fmt::Debug + Send + Sync {
	fn inner(&self) -> &Child;
	fn inner_mut(&mut self) -> &mut Child;
	fn into_inner(self: Box<Self>) -> Child;

	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		&mut self.inner_mut().stdin
	}

	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		&mut self.inner_mut().stdout
	}

	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		&mut self.inner_mut().stderr
	}

	fn id(&self) -> u32 {
		self.inner().id()
	}

	fn kill(&mut self) -> Result<()> {
		self.start_kill()?;
		self.wait()?;
		Ok(())
	}

	fn start_kill(&mut self) -> Result<()> {
		self.inner_mut().start_kill()
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	fn wait(&mut self) -> Result<ExitStatus> {
		self.inner_mut().wait()
	}

	fn wait_with_output(self: Box<Self>) -> Result<Output>
	where
		Self: 'static,
	{
		todo!()
	}

	#[cfg(unix)]
	fn signal(&self, sig: Signal) -> Result<()> {
		kill(
			Pid::from_raw(i32::try_from(self.id()).map_err(std::io::Error::other)?),
			sig,
		)
		.map_err(std::io::Error::from)
	}
}

impl StdChildWrapper for Child {
	fn inner(&self) -> &Child {
		self
	}
	fn inner_mut(&mut self) -> &mut Child {
		self
	}
	fn into_inner(self: Box<Self>) -> Child {
		*self
	}
}
