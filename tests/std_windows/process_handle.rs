use std::{
	any::TypeId,
	fs,
	os::windows::{
		io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle},
		process::ExitStatusExt,
	},
	path::{Path, PathBuf},
	process::{Command, Command as StdCommand, ExitStatus},
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicUsize, Ordering},
		mpsc,
	},
	time::Instant,
};

use super::{prelude::*, windows_thread::ProcessGuard};

const JOB_WAIT_DESCENDANT_PID: &str = "PROCESS_WRAP_JOB_WAIT_DESCENDANT_PID";
const JOB_WAIT_DESCENDANT_RELEASE: &str = "PROCESS_WRAP_JOB_WAIT_DESCENDANT_RELEASE";
const JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

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
struct ExactResumeChild {
	child: std::process::Child,
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
		Some(self.child.as_handle())
	}

	fn resume_after_job_assignment(&mut self) -> Option<Result<()>> {
		self.resumes.fetch_add(1, Ordering::SeqCst);
		Some(Ok(()))
	}

	fn id(&self) -> u32 {
		self.child.id()
	}

	fn start_kill(&mut self) -> Result<()> {
		self.child.kill()
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.child.try_wait()
	}

	fn wait(&mut self) -> Result<ExitStatus> {
		self.child.wait()
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
		let process = child.as_handle().try_clone_to_owned()?;
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
		// SAFETY: `LegacyInline` adds no cleanup or supervision state.
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
fn job_object_finds_terminal_capabilities_below_multiple_legacy_layers() -> Result<()> {
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
	let _ = child.wait()?;
	Ok(())
}

#[test]
fn provider_job_assignment_precedes_commit_in_both_orders() -> Result<()> {
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
		let _ = child.wait()?;
	}
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

#[derive(Debug)]
struct ReleaseOnDrop(PathBuf);

impl Drop for ReleaseOnDrop {
	fn drop(&mut self) {
		let _ = fs::File::create(&self.0);
	}
}

#[derive(Debug)]
struct WaiterGuard {
	release_file: PathBuf,
	thread: Option<std::thread::JoinHandle<()>>,
}

impl WaiterGuard {
	fn join(&mut self) -> Result<()> {
		if let Some(thread) = self.thread.take() {
			thread
				.join()
				.map_err(|_| std::io::Error::other("JobObject wait thread panicked"))?;
		}
		Ok(())
	}
}

impl Drop for WaiterGuard {
	fn drop(&mut self) {
		let _ = fs::File::create(&self.release_file);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

fn wait_for_pid(path: &Path) -> Result<u32> {
	let deadline = Instant::now() + JOB_WAIT_TIMEOUT;
	loop {
		if let Ok(pid) = fs::read_to_string(path)
			.and_then(|pid| pid.trim().parse().map_err(std::io::Error::other))
		{
			return Ok(pid);
		}
		if Instant::now() >= deadline {
			return Err(std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				"descendant helper did not report its process ID",
			));
		}
		sleep(Duration::from_millis(10));
	}
}

fn wait_for_process_exit(guard: ProcessGuard) -> Result<()> {
	let deadline = Instant::now() + JOB_WAIT_TIMEOUT;
	loop {
		if guard.has_exited()? {
			return guard.disarm();
		}
		if Instant::now() >= deadline {
			return Err(std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				"process did not exit before the test deadline",
			));
		}
		sleep(Duration::from_millis(10));
	}
}

fn descendant_job_command(pid_file: &Path, release_file: &Path) -> Result<CommandWrap> {
	Ok(CommandWrap::with_new(std::env::current_exe()?, |command| {
		command
			.args([
				"--exact",
				"std_windows::process_handle::job_wait_direct_child_helper",
				"--ignored",
				"--nocapture",
			])
			.env(JOB_WAIT_DESCENDANT_PID, pid_file)
			.env(JOB_WAIT_DESCENDANT_RELEASE, release_file)
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null());
	}))
}

#[test]
#[ignore = "subprocess helper"]
fn job_wait_descendant_helper() -> Result<()> {
	let release = PathBuf::from(
		std::env::var_os(JOB_WAIT_DESCENDANT_RELEASE)
			.ok_or_else(|| std::io::Error::other("descendant release path is missing"))?,
	);
	let deadline = Instant::now() + JOB_WAIT_TIMEOUT;
	while !release.exists() {
		if Instant::now() >= deadline {
			return Err(std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				"descendant release was not signaled",
			));
		}
		sleep(Duration::from_millis(10));
	}
	Ok(())
}

#[test]
#[ignore = "subprocess helper"]
fn job_wait_direct_child_helper() -> Result<()> {
	let pid_file = PathBuf::from(
		std::env::var_os(JOB_WAIT_DESCENDANT_PID)
			.ok_or_else(|| std::io::Error::other("descendant PID path is missing"))?,
	);
	let descendant = StdCommand::new(std::env::current_exe()?)
		.args([
			"--exact",
			"std_windows::process_handle::job_wait_descendant_helper",
			"--ignored",
			"--nocapture",
		])
		.env(
			JOB_WAIT_DESCENDANT_RELEASE,
			std::env::var_os(JOB_WAIT_DESCENDANT_RELEASE)
				.ok_or_else(|| std::io::Error::other("descendant release path is missing"))?,
		)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()?;
	fs::write(pid_file, descendant.id().to_string())?;
	drop(descendant);
	Ok(())
}

#[test]
fn job_waits_for_descendant_after_direct_child_exits() -> Result<()> {
	let directory = tempfile::tempdir()?;
	let pid_file = directory.path().join("descendant.pid");
	let release_file = directory.path().join("release");
	let _release_on_drop = ReleaseOnDrop(release_file.clone());
	let mut command = descendant_job_command(&pid_file, &release_file)?;
	command.wrap(JobObject);
	let mut child = command.spawn()?;
	let direct_process = ProcessGuard::open(child.id())?;
	let descendant_process = ProcessGuard::open(wait_for_pid(&pid_file)?)?;
	wait_for_process_exit(direct_process)?;

	assert_eq!(
		child.try_wait()?,
		None,
		"direct-child exit must remain pending while the job has a live descendant"
	);

	let (drain_entered, drain_entry) = mpsc::channel();
	let (completed, completion) = mpsc::channel();
	let waiter_release = release_file.clone();
	let thread = std::thread::spawn(move || {
		let pending = child.try_wait();
		let entered = match &pending {
			Ok(None) => Ok(()),
			Ok(Some(_)) => Err(std::io::Error::other(
				"blocking wait setup observed a drained job before release",
			)),
			Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
		};
		let _ = drain_entered.send(entered);
		let result = match pending {
			Ok(None) => child.wait(),
			Ok(Some(status)) => Ok(status),
			Err(error) => Err(error),
		};
		let _ = completed.send((result, child));
	});
	let mut waiter = WaiterGuard {
		release_file: waiter_release,
		thread: Some(thread),
	};
	drain_entry
		.recv_timeout(JOB_WAIT_TIMEOUT)
		.map_err(|error| {
			std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				format!("waiter did not enter job drain after a false query: {error}"),
			)
		})??;
	fs::File::create(&release_file)?;
	let (status, mut child) = completion.recv_timeout(JOB_WAIT_TIMEOUT).map_err(|error| {
		std::io::Error::new(
			std::io::ErrorKind::TimedOut,
			format!("JobObject wait did not finish after descendant release: {error}"),
		)
	})?;
	waiter.join()?;
	let status = status?;
	assert!(status.success());
	assert_eq!(child.try_wait()?, Some(status));
	assert_eq!(child.wait()?, status);
	wait_for_process_exit(descendant_process)?;
	Ok(())
}
