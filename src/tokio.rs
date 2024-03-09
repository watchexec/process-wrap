use std::any::TypeId;

use indexmap::IndexMap;
use tokio::process::Command;
use tracing::debug;

pub struct TokioCommandWrap {
	command: Command,
	wrappers: IndexMap<TypeId, Box<dyn TokioCommandWrapper>>,
}

impl TokioCommandWrap {
	pub fn new(command: Command) -> Self {
		Self { command, wrappers: IndexMap::new() }
	}

	pub fn wrap<W: TokioCommandWrapper + 'static>(&mut self, wrapper: W) {
		let typeid = TypeId::of::<W>();
		let mut wrapper = Some(Box::new(wrapper));
		let extant = self.wrappers.entry(typeid).or_insert_with(|| wrapper.take().unwrap());
		if let Some(wrapper) = wrapper {
			extant.extend(wrapper);
		}
	}

	pub fn spawn(mut self) -> tokio::io::Result<Box<dyn TokioChildWrapper>> {
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
	// it is guaranteed that `other` will be of the same type as `self`
	fn extend(&mut self, other: Box<dyn TokioCommandWrapper>);

	fn pre_spawn(&mut self, command: &mut Command) -> tokio::io::Result<()>;
	fn post_spawn(&mut self, child: &mut tokio::process::Child) -> tokio::io::Result<()>;

	fn wrap_child(&mut self, child: Box<dyn TokioChildWrapper>) -> tokio::io::Result<Box<dyn TokioChildWrapper>> {
		Ok(child)
	}

	fn wait(&mut self, child: &mut tokio::process::Child) -> tokio::io::Result<std::process::ExitStatus>;
}

pub trait TokioChildWrapper: std::fmt::Debug {}
impl TokioChildWrapper for tokio::process::Child {}
