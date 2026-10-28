use std::io::Result;

use crate::std::ChildWrapper;

use super::{CommandWrap, CommandWrapper};

/// Wrapper which ensures a [`Command`](std::process::Command) is killed on drop.
///
/// This wrapper exists to be able to install a drop guard on a [`Command`](std::process::Command) and also store that
/// fact in the wrapper, so that it can be used by other wrappers. .
/// Notably this is used by the `JobObject` wrapper.
///
/// This wrapper should be the last wrapper in order to properly call the kill function of all other wrappers.
///
/// # Example
///
/// ```rust,no_run
/// use std::{io, process::{Stdio, Command}};
/// use process_wrap::std::{CommandWrap, KillOnDrop};
///
/// let child = CommandWrap::from(Command::new("yes"))
/// .wrap(KillOnDrop)
/// .spawn()?;
/// let dead_pid = child.id();
/// drop(child);
/// // The `yes` subprocess was killed by the drop guard.
/// # Ok::<(), io::Error>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct KillOnDrop;

/// Wrapper for `Child` which ensures that the child is killed on drop.
#[derive(Debug)]
struct KillOnDropChild {
	inner: Option<Box<dyn ChildWrapper>>,
}

impl KillOnDropChild {
	#[must_use]
	fn new(inner: Box<dyn ChildWrapper>) -> Self {
		Self { inner: Some(inner) }
	}
}

impl CommandWrapper for KillOnDrop {
	fn wrap_child(
		&mut self,
		inner: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		Ok(Box::new(KillOnDropChild::new(inner)))
	}
}

impl ChildWrapper for KillOnDropChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref().unwrap().as_ref()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut().unwrap().as_mut()
	}
	fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner.take().unwrap()
	}
}

impl Drop for KillOnDropChild {
	fn drop(&mut self) {
		let Some(inner) = self.inner.as_mut() else {
			return;
		};
		let _ = inner.kill();
	}
}
