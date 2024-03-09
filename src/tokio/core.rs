use std::{
	any::TypeId,
	future::Future,
	io::{Error, Result},
	process::ExitStatus,
};

use indexmap::IndexMap;
#[cfg(unix)]
use nix::{
	sys::signal::{kill, Signal},
	unistd::Pid,
};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tracing::debug;

pub struct TokioCommandWrap {
	command: Command,
	wrappers: IndexMap<TypeId, Box<dyn TokioCommandWrapper>>,
}

impl TokioCommandWrap {
	pub fn new(command: Command) -> Self {
		Self {
			command,
			wrappers: IndexMap::new(),
		}
	}

	pub fn wrap<W: TokioCommandWrapper + 'static>(&mut self, wrapper: W) {
		let typeid = TypeId::of::<W>();
		let mut wrapper = Some(Box::new(wrapper));
		let extant = self
			.wrappers
			.entry(typeid)
			.or_insert_with(|| wrapper.take().unwrap());
		if let Some(wrapper) = wrapper {
			extant.extend(wrapper);
		}
	}

	pub fn spawn(mut self) -> Result<Box<dyn TokioChildWrapper>> {
		let mut command = self.command;
		for (id, wrapper) in &mut self.wrappers {
			debug!(?id, "pre_spawn");
			wrapper.pre_spawn(&mut command)?;
		}

		let mut child = command.spawn()?;
		for (id, wrapper) in &mut self.wrappers {
			debug!(?id, "post_spawn");
			wrapper.post_spawn(&mut child)?;
		}

		let mut child = Box::new(child) as Box<dyn TokioChildWrapper>;
		for (id, wrapper) in &mut self.wrappers {
			debug!(?id, "wrap_child");
			child = wrapper.wrap_child(child)?;
		}

		Ok(child)
	}
}

pub trait TokioCommandWrapper: std::fmt::Debug {
	// process-wrap guarantees that `other` will be of the same type as `self`
	// note that other crates that may use this trait should guarantee this, but
	// that cannot be enforced by the type system, so you should still panic if
	// downcasting fails, instead of potentially causing UB
	fn extend(&mut self, _other: Box<dyn TokioCommandWrapper>) {}

	fn pre_spawn(&mut self, _command: &mut Command) -> Result<()> {
		Ok(())
	}

	fn post_spawn(&mut self, _child: &mut tokio::process::Child) -> Result<()> {
		Ok(())
	}

	fn wrap_child(
		&mut self,
		child: Box<dyn TokioChildWrapper>,
	) -> Result<Box<dyn TokioChildWrapper>> {
		Ok(child)
	}
}

pub trait TokioChildWrapper: std::fmt::Debug {
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

	fn id(&self) -> Option<u32> {
		self.inner().id()
	}

	fn kill(&mut self) -> Box<dyn Future<Output = Result<()>> + '_> {
		Box::new(self.inner_mut().kill())
	}

	fn start_kill(&mut self) -> Result<()> {
		self.inner_mut().start_kill()
	}

	fn wait(&mut self) -> Box<dyn Future<Output = Result<ExitStatus>> + '_> {
		Box::new(self.inner_mut().wait())
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	#[cfg(unix)]
	fn signal(&self, sig: Signal) -> Result<()> {
		if let Some(id) = self.id() {
			kill(Pid::from_raw(i32::try_from(id).map_err(Error::other)?), sig).map_err(Error::from)
		} else {
			Ok(())
		}
	}
}

impl TokioChildWrapper for Child {
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
