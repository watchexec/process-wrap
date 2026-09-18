use std::{
	any::TypeId,
	future::Future,
	os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle},
	pin::Pin,
	process::ExitStatus,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
};

use super::prelude::*;

#[link(name = "kernel32")]
unsafe extern "system" {
	fn TerminateProcess(process: *mut std::ffi::c_void, exit_code: u32) -> i32;
	fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
}

fn terminate_and_wait(process: &OwnedHandle) {
	let raw = process.as_raw_handle();
	// SAFETY: the transaction owns this process handle until both calls return.
	let _ = unsafe { TerminateProcess(raw, 1) };
	// SAFETY: the process handle remains live for the duration of this call.
	let _ = unsafe { WaitForSingleObject(raw, u32::MAX) };
}

#[derive(Debug)]
struct OpaqueChild {
	inner_calls: Arc<AtomicUsize>,
	killed: Arc<AtomicBool>,
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
}

fn opaque_child() -> (Box<dyn ChildWrapper>, Arc<AtomicUsize>, Arc<AtomicBool>) {
	let inner_calls = Arc::new(AtomicUsize::new(0));
	let killed = Arc::new(AtomicBool::new(false));
	(
		Box::new(OpaqueChild {
			inner_calls: Arc::clone(&inner_calls),
			killed: Arc::clone(&killed),
		}),
		inner_calls,
		killed,
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
struct ExactResumeChild {
	child: tokio::process::Child,
	resumes: Arc<AtomicUsize>,
}

impl ChildWrapper for ExactResumeChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		let handle = self.child.raw_handle()?;
		// SAFETY: the child owns this handle and the returned borrow cannot outlive `self`.
		Some(unsafe { BorrowedHandle::borrow_raw(handle) })
	}

	fn resume_after_job_assignment(&mut self) -> Option<Result<()>> {
		self.resumes.fetch_add(1, Ordering::SeqCst);
		Some(Ok(()))
	}

	fn id(&self) -> Option<u32> {
		self.child.id()
	}

	fn start_kill(&mut self) -> Result<()> {
		self.child.start_kill()
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.child.try_wait()
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
		Box::pin(self.child.wait())
	}
}

#[derive(Debug)]
struct CommitAfterResume {
	resumes: Arc<AtomicUsize>,
	commits: Arc<AtomicUsize>,
	process: Option<OwnedHandle>,
}

impl SpawnTransaction for CommitAfterResume {
	fn commit(&mut self) -> Result<()> {
		assert_eq!(self.resumes.load(Ordering::SeqCst), 1);
		self.commits.fetch_add(1, Ordering::SeqCst);
		self.process.take();
		Ok(())
	}

	fn rollback(&mut self) -> Result<()> {
		if let Some(process) = self.process.take() {
			terminate_and_wait(&process);
		}
		Ok(())
	}
}

#[derive(Debug)]
struct ProcessProvider {
	resumes: Arc<AtomicUsize>,
	commits: Arc<AtomicUsize>,
}

impl SpawnProvider for ProcessProvider {
	fn spawn(&self, attempt: &mut SpawnAttempt, _command: &CommandWrap) -> Result<ProviderProduct> {
		assert!(!attempt.is_native_only());
		let policy = attempt.windows_spawn_policy();
		assert!(policy.has_job_object());
		assert!(policy.is_temporarily_suspended());
		let child = sleeping_command().spawn()?;
		let handle = child
			.raw_handle()
			.ok_or_else(|| std::io::Error::other("spawned child has no process handle"))?;
		// SAFETY: `child` owns this handle until it is moved into `ExactResumeChild` below.
		let process = unsafe { BorrowedHandle::borrow_raw(handle) }.try_clone_to_owned()?;
		Ok(ProviderProduct::new(
			Box::new(ExactResumeChild {
				child,
				resumes: Arc::clone(&self.resumes),
			}),
			Box::new(CommitAfterResume {
				resumes: Arc::clone(&self.resumes),
				commits: Arc::clone(&self.commits),
				process: Some(process),
			}),
		))
	}
}

#[derive(Debug)]
struct ProviderWrapper(ProcessProvider);

impl CommandWrapper for ProviderWrapper {
	fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
		Some(&self.0)
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
struct LegacyInlineChild(tokio::process::Child);

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

fn sleeping_command() -> tokio::process::Command {
	let mut command = tokio::process::Command::new("cmd.exe");
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
	let (child, inner_calls, _) = opaque_child();
	assert!(child.process_handle().is_none());
	assert_eq!(inner_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn job_object_rejects_an_opaque_child_and_cleans_it_up() {
	let (child, inner_calls, killed) = opaque_child();
	let core = CommandWrap::with_new("cmd.exe", |_| {});
	let error = JobObject.wrap_child(child, &core).unwrap_err();

	assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
	assert_eq!(inner_calls.load(Ordering::SeqCst), 1);
	assert!(killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn transparent_child_delegates_the_native_process_handle() -> Result<()> {
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
	let _ = child.wait().await?;
	assert_eq!(delegated_handle, native_handle);
	Ok(())
}

#[tokio::test]
async fn job_object_falls_back_through_a_legacy_transparent_child() -> Result<()> {
	let mut command = sleeping_command_wrap();
	command.wrap(LegacyTransparent).wrap(JobObject);
	let mut child = command.spawn()?;

	let direct_type = child.inner().type_id();
	let has_handle = child.process_handle().is_some();
	child.start_kill()?;
	let _ = child.wait().await?;

	assert_eq!(direct_type, TypeId::of::<LegacyTransparentChild>());
	assert!(has_handle);
	Ok(())
}

#[tokio::test]
async fn job_object_finds_terminal_capabilities_below_multiple_legacy_layers() -> Result<()> {
	let resumes = Arc::new(AtomicUsize::new(0));
	let terminal: Box<dyn ChildWrapper> = Box::new(ExactResumeChild {
		child: sleeping_command().spawn()?,
		resumes: Arc::clone(&resumes),
	});
	let child: Box<dyn ChildWrapper> = Box::new(LegacyTransparentChild {
		inner: Box::new(LegacyTransparentChild { inner: terminal }),
	});
	let core = CommandWrap::new("cmd.exe");
	let mut child = JobObject.wrap_child(child, &core)?;

	assert_eq!(resumes.load(Ordering::SeqCst), 1);
	assert!(child.process_handle().is_some());
	child.start_kill()?;
	let _ = child.wait().await?;
	Ok(())
}

#[tokio::test]
async fn provider_job_assignment_precedes_commit_in_both_orders() -> Result<()> {
	for provider_first in [false, true] {
		let resumes = Arc::new(AtomicUsize::new(0));
		let commits = Arc::new(AtomicUsize::new(0));
		let provider = ProviderWrapper(ProcessProvider {
			resumes: Arc::clone(&resumes),
			commits: Arc::clone(&commits),
		});
		let mut command = CommandWrap::new("provider-owned-program");
		if provider_first {
			command.wrap(provider).wrap(JobObject);
		} else {
			command.wrap(JobObject).wrap(provider);
		}

		let mut child = command.spawn()?;
		assert_eq!(resumes.load(Ordering::SeqCst), 1);
		assert_eq!(commits.load(Ordering::SeqCst), 1);
		child.start_kill()?;
		let _ = child.wait().await?;
	}
	Ok(())
}

#[tokio::test]
async fn job_object_falls_back_through_a_legacy_inline_child() -> Result<()> {
	let mut command = sleeping_command_wrap();
	command.wrap(LegacyInline).wrap(JobObject);
	let mut child = command.spawn()?;

	let direct_type = child.inner().type_id();
	let has_handle = child.process_handle().is_some();
	child.start_kill()?;
	let _ = child.wait().await?;

	assert_eq!(direct_type, TypeId::of::<LegacyInlineChild>());
	assert!(has_handle);
	Ok(())
}

#[tokio::test]
async fn job_object_uses_delegated_handle_and_preserves_the_direct_child() -> Result<()> {
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
	let _ = direct.wait().await?;

	assert!(outer_has_handle);
	assert_eq!(direct_type, TypeId::of::<TransparentChild>());
	assert_eq!(direct_mut_type, TypeId::of::<TransparentChild>());
	assert_eq!(consumed_type, TypeId::of::<TransparentChild>());
	assert!(consumed_has_handle);
	Ok(())
}
