use std::{
	any::TypeId,
	os::windows::{
		io::{AsRawHandle, BorrowedHandle},
		process::ExitStatusExt,
	},
	process::{Command, ExitStatus},
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
};

use super::prelude::*;

#[derive(Debug)]
struct OpaqueChild {
	inner_calls: Arc<AtomicUsize>,
	killed: Arc<AtomicBool>,
	waited: Arc<AtomicBool>,
}

impl ChildWrapper for OpaqueChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner_calls.fetch_add(1, Ordering::SeqCst);
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn start_kill(&mut self) -> Result<()> {
		self.killed.store(true, Ordering::SeqCst);
		Ok(())
	}

	fn wait(&mut self) -> Result<ExitStatus> {
		self.waited.store(true, Ordering::SeqCst);
		Ok(ExitStatus::from_raw(0))
	}
}

fn opaque_child() -> (
	Box<dyn ChildWrapper>,
	Arc<AtomicUsize>,
	Arc<AtomicBool>,
	Arc<AtomicBool>,
) {
	let inner_calls = Arc::new(AtomicUsize::new(0));
	let killed = Arc::new(AtomicBool::new(false));
	let waited = Arc::new(AtomicBool::new(false));
	(
		Box::new(OpaqueChild {
			inner_calls: Arc::clone(&inner_calls),
			killed: Arc::clone(&killed),
			waited: Arc::clone(&waited),
		}),
		inner_calls,
		killed,
		waited,
	)
}

#[derive(Debug)]
struct TransparentChild {
	inner: Box<dyn ChildWrapper>,
}

impl ChildWrapper for TransparentChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}

	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		self.inner.process_handle()
	}
}

#[derive(Debug)]
struct Transparent;

impl CommandWrapper for Transparent {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		Ok(Box::new(TransparentChild { inner: child }))
	}
}

#[derive(Debug)]
struct LegacyTransparentChild {
	inner: Box<dyn ChildWrapper>,
}

impl ChildWrapper for LegacyTransparentChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}
}

#[derive(Debug)]
struct LegacyTransparent;

impl CommandWrapper for LegacyTransparent {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		Ok(Box::new(LegacyTransparentChild { inner: child }))
	}
}

#[repr(transparent)]
#[derive(Debug)]
struct LegacyInlineChild(std::process::Child);

impl ChildWrapper for LegacyInlineChild {
	fn inner(&self) -> &dyn ChildWrapper {
		&self.0
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		&mut self.0
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		Box::new(self.0)
	}
}

#[derive(Debug)]
struct LegacyInline;

impl CommandWrapper for LegacyInline {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let child = unsafe { child.try_into_inner_child() }
			.map_err(|_| std::io::Error::other("legacy inline wrapper expected a native child"))?;
		Ok(Box::new(LegacyInlineChild(child)))
	}
}

fn sleeping_command() -> Command {
	let mut command = Command::new("cmd.exe");
	command.args(["/D", "/S", "/C", "ping -n 6 127.0.0.1 >NUL"]);
	command
}

fn sleeping_command_wrap() -> CommandWrap {
	CommandWrap::with_new("cmd.exe", |command| {
		command.args(["/D", "/S", "/C", "ping -n 6 127.0.0.1 >NUL"]);
	})
}

#[test]
fn opaque_child_defaults_to_no_process_handle_without_traversing() {
	let (child, inner_calls, _, _) = opaque_child();
	assert!(child.process_handle().is_none());
	assert_eq!(inner_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn job_object_rejects_an_opaque_child_and_cleans_it_up() {
	let (child, inner_calls, killed, waited) = opaque_child();
	let core = CommandWrap::with_new("cmd.exe", |_| {});
	let error = JobObject.wrap_child(child, &core).unwrap_err();

	assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
	assert_eq!(inner_calls.load(Ordering::SeqCst), 1);
	assert!(killed.load(Ordering::SeqCst));
	assert!(waited.load(Ordering::SeqCst));
}

#[test]
fn transparent_child_delegates_the_native_process_handle() -> Result<()> {
	let native = sleeping_command().spawn()?;
	let native_handle = native
		.process_handle()
		.expect("a native child exposes its process handle")
		.as_raw_handle();
	let mut child: Box<dyn ChildWrapper> = Box::new(TransparentChild {
		inner: Box::new(native),
	});
	let delegated_handle = child
		.process_handle()
		.expect("a transparent child delegates its process handle")
		.as_raw_handle();

	child.start_kill()?;
	let _ = child.wait()?;
	assert_eq!(delegated_handle, native_handle);
	Ok(())
}

#[test]
fn job_object_falls_back_through_a_legacy_transparent_child() -> Result<()> {
	let mut command = sleeping_command_wrap();
	command.wrap(LegacyTransparent).wrap(JobObject);
	let mut child = command.spawn()?;

	let direct_type = child.inner().type_id();
	let has_handle = child.process_handle().is_some();
	child.start_kill()?;
	let _ = child.wait()?;

	assert_eq!(direct_type, TypeId::of::<LegacyTransparentChild>());
	assert!(has_handle);
	Ok(())
}

#[test]
fn job_object_falls_back_through_a_legacy_inline_child() -> Result<()> {
	let mut command = sleeping_command_wrap();
	command.wrap(LegacyInline).wrap(JobObject);
	let mut child = command.spawn()?;

	let direct_type = child.inner().type_id();
	let has_handle = child.process_handle().is_some();
	child.start_kill()?;
	let _ = child.wait()?;

	assert_eq!(direct_type, TypeId::of::<LegacyInlineChild>());
	assert!(has_handle);
	Ok(())
}

#[test]
fn job_object_uses_delegated_handle_and_preserves_the_direct_child() -> Result<()> {
	let mut command = sleeping_command_wrap();
	command.wrap(Transparent).wrap(JobObject);
	let mut child = command.spawn()?;

	let outer_has_handle = child.process_handle().is_some();
	let direct_type = child.inner().type_id();
	let direct_mut_type = child.inner_mut().type_id();
	let mut direct = child.into_inner();
	let consumed_type = direct.as_ref().type_id();
	let consumed_has_handle = direct.process_handle().is_some();

	direct.start_kill()?;
	let _ = direct.wait()?;

	assert!(outer_has_handle);
	assert_eq!(direct_type, TypeId::of::<TransparentChild>());
	assert_eq!(direct_mut_type, TypeId::of::<TransparentChild>());
	assert_eq!(consumed_type, TypeId::of::<TransparentChild>());
	assert!(consumed_has_handle);
	Ok(())
}
