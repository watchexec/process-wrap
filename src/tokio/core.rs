use std::{
	any::{Any, TypeId},
	ffi::OsStr,
	future::Future,
	io::{Error, Result},
	mem::{replace, take},
	process::{ExitStatus, Output},
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
	pub fn with_new(program: impl AsRef<OsStr>, init: impl FnOnce(&mut Command)) -> Self {
		let mut command = Command::new(program);
		init(&mut command);
		Self {
			command,
			wrappers: IndexMap::new(),
		}
	}

	pub fn wrap<W: TokioCommandWrapper + 'static>(&mut self, wrapper: W) -> &mut Self {
		let typeid = TypeId::of::<W>();
		let mut wrapper = Some(Box::new(wrapper));
		let extant = self
			.wrappers
			.entry(typeid)
			.or_insert_with(|| wrapper.take().unwrap());
		if let Some(wrapper) = wrapper {
			extant.extend(wrapper);
		}

		self
	}

	// poor man's try..finally block
	#[inline]
	fn spawn_inner(
		&self,
		command: &mut Command,
		wrappers: &mut IndexMap<TypeId, Box<dyn TokioCommandWrapper>>,
	) -> Result<Box<dyn TokioChildWrapper>> {
		for (id, wrapper) in wrappers.iter_mut() {
			debug!(?id, "pre_spawn");
			wrapper.pre_spawn(command, self)?;
		}

		let mut child = command.spawn()?;
		for (id, wrapper) in wrappers.iter_mut() {
			debug!(?id, "post_spawn");
			wrapper.post_spawn(&mut child, self)?;
		}

		let mut child = Box::new(child) as Box<dyn TokioChildWrapper>;
		for (id, wrapper) in wrappers.iter_mut() {
			debug!(?id, "wrap_child");
			child = wrapper.wrap_child(child, self)?;
		}

		Ok(child)
	}

	pub fn spawn(&mut self) -> Result<Box<dyn TokioChildWrapper>> {
		let mut command = replace(&mut self.command, Command::new(""));
		let mut wrappers = take(&mut self.wrappers);

		let res = self.spawn_inner(&mut command, &mut wrappers);

		self.command = command;
		self.wrappers = wrappers;

		res
	}

	pub fn has_wrap<W: TokioCommandWrapper + 'static>(&self) -> bool {
		let typeid = TypeId::of::<W>();
		self.wrappers.contains_key(&typeid)
	}

	pub fn get_wrap<W: TokioCommandWrapper + 'static>(&self) -> Option<&W> {
		let typeid = TypeId::of::<W>();
		self.wrappers.get(&typeid).map(|w| {
			let w_any = w as &dyn Any;
			w_any
				.downcast_ref()
				.expect("downcasting is guaranteed to succeed due to wrap()'s internals")
		})
	}
}

impl From<Command> for TokioCommandWrap {
	fn from(command: Command) -> Self {
		Self {
			command,
			wrappers: IndexMap::new(),
		}
	}
}

pub trait TokioCommandWrapper: std::fmt::Debug {
	// process-wrap guarantees that `other` will be of the same type as `self`
	// note that other crates that may use this trait should guarantee this, but
	// that cannot be enforced by the type system, so you should still panic if
	// downcasting fails, instead of potentially causing UB
	fn extend(&mut self, _other: Box<dyn TokioCommandWrapper>) {}

	fn pre_spawn(&mut self, _command: &mut Command, _core: &TokioCommandWrap) -> Result<()> {
		Ok(())
	}

	fn post_spawn(
		&mut self,
		_child: &mut tokio::process::Child,
		_core: &TokioCommandWrap,
	) -> Result<()> {
		Ok(())
	}

	fn wrap_child(
		&mut self,
		child: Box<dyn TokioChildWrapper>,
		_core: &TokioCommandWrap,
	) -> Result<Box<dyn TokioChildWrapper>> {
		Ok(child)
	}
}

pub trait TokioChildWrapper: std::fmt::Debug + Send + Sync {
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

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	fn wait(&mut self) -> Box<dyn Future<Output = Result<ExitStatus>> + '_> {
		Box::new(self.inner_mut().wait())
	}

	fn wait_with_output(self: Box<Self>) -> Box<dyn Future<Output = Result<Output>>> {
		Box::new(self.into_inner().wait_with_output())
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
