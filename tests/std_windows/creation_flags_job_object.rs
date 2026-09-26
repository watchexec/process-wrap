use std::{
	fs,
	io::{Error, ErrorKind},
	os::windows::process::CommandExt,
	panic::{AssertUnwindSafe, catch_unwind},
	path::PathBuf,
	process::{Command as StdCommand, ExitStatus, Stdio},
	sync::{
		Arc, Mutex,
		atomic::{AtomicU32, Ordering},
	},
	time::Instant,
};

#[cfg(feature = "tracing")]
use tracing::{
	Event, Metadata, Subscriber,
	field::{Field, Visit},
	span::{Attributes, Id, Record},
};

use windows::Win32::System::Threading::{
	CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, DETACHED_PROCESS,
	PROCESS_CREATION_FLAGS,
};

use super::{
	prelude::*,
	windows_thread::{ProcessGuard, process_has_suspended_thread, resume_process_threads},
};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const DESCENDANT_PID_FILE: &str = "PROCESS_WRAP_DESCENDANT_PID_FILE";
#[cfg(feature = "tracing")]
const FINAL_OWNER_TRACE_HELPER: &str = "PROCESS_WRAP_FINAL_OWNER_TRACE_HELPER";
static PID_FILE_SEQUENCE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
enum Order {
	CreationFlagsFirst,
	JobObjectFirst,
}

#[derive(Clone, Copy, Debug)]
struct ExpectedPolicy {
	user_flags: u32,
	spawn_flags: u32,
	has_creation_flags: bool,
	has_job_object: bool,
	explicit_suspension: bool,
	temporary_suspension: bool,
}

#[derive(Debug)]
struct InspectProvider(ExpectedPolicy);

impl SpawnProvider for InspectProvider {
	fn validate_attempt(&self, attempt: &SpawnAttempt, _command: &CommandWrap) -> Result<()> {
		assert!(!attempt.is_native_only());
		assert!(!attempt.kills_on_drop());
		let policy = attempt.windows_spawn_policy();
		assert_eq!(policy.user_creation_flags(), self.0.user_flags);
		assert_eq!(policy.spawn_creation_flags(), self.0.spawn_flags);
		assert_eq!(policy.has_creation_flags(), self.0.has_creation_flags);
		assert_eq!(policy.has_job_object(), self.0.has_job_object);
		assert_eq!(policy.is_explicitly_suspended(), self.0.explicit_suspension);
		assert_eq!(
			policy.is_temporarily_suspended(),
			self.0.temporary_suspension
		);
		assert!(!policy.kills_on_drop());
		Err(Error::other("policy inspected"))
	}

	fn spawn(
		&self,
		_attempt: &mut SpawnAttempt,
		_command: &CommandWrap,
	) -> Result<ProviderProduct> {
		unreachable!("attempt validation stops before provider allocation")
	}
}

#[derive(Debug)]
struct InspectPolicy(InspectProvider);

impl InspectPolicy {
	fn new(expected: ExpectedPolicy) -> Self {
		Self(InspectProvider(expected))
	}
}

impl CommandWrapper for InspectPolicy {
	fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
		Some(&self.0)
	}
}

#[derive(Clone, Copy, Debug)]
enum Failure {
	Error,
	Panic,
}

#[derive(Debug)]
struct CapturePid(Arc<AtomicU32>);

impl CommandWrapper for CapturePid {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_core: &CommandWrap,
	) -> Result<()> {
		self.0.store(child.id(), Ordering::SeqCst);
		Ok(())
	}
}

#[derive(Debug)]
struct FailWrapOnce {
	failure: Failure,
	failed: bool,
}

impl CommandWrapper for FailWrapOnce {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		if self.failed {
			return Ok(child);
		}
		self.failed = true;
		match self.failure {
			Failure::Error => Err(Error::other("child wrapping failed")),
			Failure::Panic => panic!("child wrapping failed"),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureHook {
	PostSpawn,
	WrapChild,
	FinalizeSpawn,
	DisarmSpawnCleanup,
	DisarmJobObject,
}

#[derive(Debug)]
struct FailFinalizationChild {
	inner: Box<dyn ChildWrapper>,
	failure: Failure,
	hook: FailureHook,
}

impl ChildWrapper for FailFinalizationChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}

	fn finalize_spawn_layer(&mut self) -> Result<()> {
		if self.hook == FailureHook::FinalizeSpawn {
			match self.failure {
				Failure::Error => Err(Error::other("child wrapping failed")),
				Failure::Panic => panic!("child wrapping failed"),
			}
		} else {
			Ok(())
		}
	}

	fn disarm_spawn_cleanup_layer(&mut self) -> Result<()> {
		if self.hook == FailureHook::DisarmSpawnCleanup {
			match self.failure {
				Failure::Error => Err(Error::other("child wrapping failed")),
				Failure::Panic => panic!("child wrapping failed"),
			}
		} else {
			Ok(())
		}
	}

	fn disarm_job_object_layer(&mut self) -> Result<()> {
		if self.hook == FailureHook::DisarmJobObject {
			match self.failure {
				Failure::Error => Err(Error::other("child wrapping failed")),
				Failure::Panic => panic!("child wrapping failed"),
			}
		} else {
			Ok(())
		}
	}
}

#[derive(Debug)]
struct FailAfterDescendant {
	failure: Failure,
	hook: FailureHook,
	unwrap_child: bool,
	pid_file: PathBuf,
	guard: Arc<Mutex<Option<ProcessGuard>>>,
}

impl FailAfterDescendant {
	fn observe_descendant(&self) -> Result<()> {
		let deadline = Instant::now() + EXIT_TIMEOUT;
		let pid = loop {
			if let Ok(pid) = fs::read_to_string(&self.pid_file)
				.and_then(|pid| pid.trim().parse().map_err(Error::other))
			{
				break pid;
			}
			if Instant::now() >= deadline {
				return Err(Error::new(
					ErrorKind::TimedOut,
					"descendant helper did not report its process ID",
				));
			}
			std::thread::sleep(Duration::from_millis(10));
		};
		*self.guard.lock().unwrap() = Some(ProcessGuard::open(pid)?);
		Ok(())
	}

	fn fail<T>(&self) -> Result<T> {
		match self.failure {
			Failure::Error => Err(Error::other("child wrapping failed")),
			Failure::Panic => panic!("child wrapping failed"),
		}
	}
}

impl CommandWrapper for FailAfterDescendant {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_core: &CommandWrap,
	) -> Result<()> {
		if self.hook != FailureHook::PostSpawn {
			return Ok(());
		}
		resume_process_threads(child.id())?;
		self.observe_descendant()?;
		self.fail()
	}

	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		if self.hook == FailureHook::PostSpawn {
			return Ok(child);
		}
		resume_process_threads(child.id())?;
		self.observe_descendant()?;
		if matches!(
			self.hook,
			FailureHook::FinalizeSpawn
				| FailureHook::DisarmSpawnCleanup
				| FailureHook::DisarmJobObject
		) {
			return Ok(Box::new(FailFinalizationChild {
				inner: child,
				failure: self.failure,
				hook: self.hook,
			}));
		}
		if self.unwrap_child {
			drop(child.into_inner());
		} else {
			drop(child);
		}
		self.fail()
	}
}

#[cfg(feature = "tracing")]
#[derive(Debug)]
struct ObserveDescendant {
	pid_file: PathBuf,
	guard: Arc<Mutex<Option<ProcessGuard>>>,
}

#[cfg(feature = "tracing")]
impl CommandWrapper for ObserveDescendant {
	fn wrap_child(
		&mut self,
		child: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		resume_process_threads(child.id())?;
		let deadline = Instant::now() + EXIT_TIMEOUT;
		let pid = loop {
			if let Ok(pid) = fs::read_to_string(&self.pid_file)
				.and_then(|pid| pid.trim().parse().map_err(Error::other))
			{
				break pid;
			}
			if Instant::now() >= deadline {
				return Err(Error::new(
					ErrorKind::TimedOut,
					"descendant helper did not report its process ID",
				));
			}
			std::thread::sleep(Duration::from_millis(10));
		};
		*self.guard.lock().unwrap() = Some(ProcessGuard::open(pid)?);
		Ok(child)
	}
}

#[cfg(feature = "tracing")]
#[derive(Default)]
struct JobLimitMessageVisitor {
	matches: bool,
	kill_on_drop: Option<bool>,
}

#[cfg(feature = "tracing")]
impl Visit for JobLimitMessageVisitor {
	fn record_bool(&mut self, field: &Field, value: bool) {
		if field.name() == "kill_on_drop" {
			self.kill_on_drop = Some(value);
		}
	}

	fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
		if field.name() == "message"
			&& format!("{value:?}").contains("setting SetInformationJobObject(limit)")
		{
			self.matches = true;
		}
	}
}

#[cfg(feature = "tracing")]
#[derive(Debug, Default)]
struct PanicOnJobDisarmEvent;

#[cfg(feature = "tracing")]
impl Subscriber for PanicOnJobDisarmEvent {
	fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
		true
	}

	fn new_span(&self, _span: &Attributes<'_>) -> Id {
		Id::from_u64(1)
	}

	fn record(&self, _span: &Id, _values: &Record<'_>) {}

	fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

	fn event(&self, event: &Event<'_>) {
		let mut visitor = JobLimitMessageVisitor::default();
		event.record(&mut visitor);
		if visitor.matches && visitor.kill_on_drop == Some(false) {
			panic!("injected final-owner tracing panic");
		}
	}

	fn enter(&self, _span: &Id) {}

	fn exit(&self, _span: &Id) {}
}

fn descendant_pid_file() -> Result<PathBuf> {
	let path = std::env::temp_dir().join(format!(
		"process-wrap-{}-{}.pid",
		std::process::id(),
		PID_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
	));
	match fs::remove_file(&path) {
		Ok(()) => {}
		Err(error) if error.kind() == ErrorKind::NotFound => {}
		Err(error) => return Err(error),
	}
	Ok(path)
}

fn wait_for_process_exit(guard: ProcessGuard) -> Result<()> {
	let deadline = Instant::now() + EXIT_TIMEOUT;
	loop {
		if guard.has_exited()? {
			return guard.disarm();
		}
		if Instant::now() >= deadline {
			return Err(Error::new(
				ErrorKind::TimedOut,
				"descendant survived the failed spawn lifecycle",
			));
		}
		sleep(Duration::from_millis(10));
	}
}

fn command(flags: PROCESS_CREATION_FLAGS, order: Order) -> CommandWrap {
	let mut command = CommandWrap::with_new("cmd.exe", |command| {
		command.args(["/D", "/S", "/C", "exit /b 0"]);
	});
	match order {
		Order::CreationFlagsFirst => {
			command.wrap(CreationFlags(flags)).wrap(JobObject);
		}
		Order::JobObjectFirst => {
			command.wrap(JobObject).wrap(CreationFlags(flags));
		}
	}
	command
}

fn wait_for_exit(child: &mut dyn ChildWrapper) -> Result<ExitStatus> {
	let deadline = Instant::now() + EXIT_TIMEOUT;
	loop {
		if let Some(status) = child.try_wait()? {
			return Ok(status);
		}
		if Instant::now() >= deadline {
			let _ = child.start_kill();
			return Err(Error::new(ErrorKind::TimedOut, "child did not exit"));
		}
		sleep(Duration::from_millis(10));
	}
}

fn assert_policy(mut command: CommandWrap, expected: ExpectedPolicy) {
	command.wrap(InspectPolicy::new(expected));
	let error = command
		.spawn()
		.expect_err("policy inspection must stop before provider allocation");
	assert_eq!(error.to_string(), "policy inspected");
}

#[test]
fn portable_policy_covers_flags_jobs_and_suspension() {
	let flags = (CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW).0;
	let explicit = (CREATE_NO_WINDOW | CREATE_SUSPENDED).0;

	let mut flags_only = CommandWrap::new("cmd.exe");
	flags_only.wrap(CreationFlags(PROCESS_CREATION_FLAGS(flags)));
	assert_policy(
		flags_only,
		ExpectedPolicy {
			user_flags: flags,
			spawn_flags: flags,
			has_creation_flags: true,
			has_job_object: false,
			explicit_suspension: false,
			temporary_suspension: false,
		},
	);

	let mut job_only = CommandWrap::new("cmd.exe");
	job_only.wrap(JobObject);
	assert_policy(
		job_only,
		ExpectedPolicy {
			user_flags: 0,
			spawn_flags: CREATE_SUSPENDED.0,
			has_creation_flags: false,
			has_job_object: true,
			explicit_suspension: false,
			temporary_suspension: true,
		},
	);

	for order in [Order::CreationFlagsFirst, Order::JobObjectFirst] {
		assert_policy(
			command(PROCESS_CREATION_FLAGS(flags), order),
			ExpectedPolicy {
				user_flags: flags,
				spawn_flags: flags | CREATE_SUSPENDED.0,
				has_creation_flags: true,
				has_job_object: true,
				explicit_suspension: false,
				temporary_suspension: true,
			},
		);
		assert_policy(
			command(PROCESS_CREATION_FLAGS(explicit), order),
			ExpectedPolicy {
				user_flags: explicit,
				spawn_flags: explicit,
				has_creation_flags: true,
				has_job_object: true,
				explicit_suspension: true,
				temporary_suspension: false,
			},
		);
	}
}

#[test]
#[ignore = "subprocess helper"]
fn lifecycle_descendant_leaf() {
	std::thread::sleep(Duration::from_secs(300));
}

#[test]
#[ignore = "subprocess helper"]
fn lifecycle_descendant_parent() {
	let mut descendant = StdCommand::new(std::env::current_exe().unwrap())
		.args(["lifecycle_descendant_leaf", "--ignored", "--nocapture"])
		.creation_flags(DETACHED_PROCESS.0)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()
		.unwrap();
	fs::write(
		std::env::var_os(DESCENDANT_PID_FILE).unwrap(),
		descendant.id().to_string(),
	)
	.unwrap();
	descendant.wait().unwrap();
}

#[test]
fn armed_job_kills_descendants_after_later_failures() -> Result<()> {
	let cases = [
		(FailureHook::PostSpawn, false, false),
		(FailureHook::WrapChild, false, false),
		(FailureHook::WrapChild, true, false),
		(FailureHook::WrapChild, true, true),
		(FailureHook::FinalizeSpawn, false, false),
		(FailureHook::FinalizeSpawn, true, false),
		(FailureHook::DisarmSpawnCleanup, false, false),
		(FailureHook::DisarmSpawnCleanup, true, false),
		(FailureHook::DisarmJobObject, false, false),
		(FailureHook::DisarmJobObject, true, false),
	];
	for failure in [Failure::Error, Failure::Panic] {
		for (hook, job_first, unwrap_child) in cases {
			let pid_file = descendant_pid_file()?;
			let guard = Arc::new(Mutex::new(None));
			let mut command = CommandWrap::with_new(std::env::current_exe()?, |command| {
				command
					.args(["lifecycle_descendant_parent", "--ignored", "--nocapture"])
					.env(DESCENDANT_PID_FILE, &pid_file);
			});
			let fail = FailAfterDescendant {
				failure,
				hook,
				unwrap_child,
				pid_file: pid_file.clone(),
				guard: Arc::clone(&guard),
			};
			if job_first {
				command.wrap(JobObject).wrap(fail);
			} else {
				command.wrap(fail).wrap(JobObject);
			}

			match failure {
				Failure::Error => {
					let error = command.spawn().expect_err("child wrapping must fail");
					assert_eq!(error.to_string(), "child wrapping failed");
				}
				Failure::Panic => {
					let panic = catch_unwind(AssertUnwindSafe(|| command.spawn()))
						.expect_err("child wrapping must panic");
					assert_eq!(
						*panic.downcast::<&'static str>().unwrap(),
						"child wrapping failed"
					);
				}
			}

			let process = guard
				.lock()
				.unwrap()
				.take()
				.expect("the failure hook opened the descendant process");
			wait_for_process_exit(process)?;
			fs::remove_file(pid_file)?;
		}
	}
	Ok(())
}

#[cfg(feature = "tracing")]
#[test]
fn final_owner_tracing_panic_leaves_job_cleanup_armed() -> Result<()> {
	if std::env::var_os(FINAL_OWNER_TRACE_HELPER).is_none() {
		let status = StdCommand::new(std::env::current_exe()?)
			.args([
				"std_windows::creation_flags_job_object::final_owner_tracing_panic_leaves_job_cleanup_armed",
				"--exact",
				"--nocapture",
			])
			.env(FINAL_OWNER_TRACE_HELPER, "1")
			.status()?;
		assert!(status.success(), "the isolated tracing regression failed");
		return Ok(());
	}

	let pid_file = descendant_pid_file()?;
	let guard = Arc::new(Mutex::new(None));
	let mut command = CommandWrap::with_new(std::env::current_exe()?, |command| {
		command
			.args(["lifecycle_descendant_parent", "--ignored", "--nocapture"])
			.env(DESCENDANT_PID_FILE, &pid_file);
	});
	command
		.wrap(ObserveDescendant {
			pid_file: pid_file.clone(),
			guard: Arc::clone(&guard),
		})
		.wrap(JobObject);

	let panic = tracing::subscriber::with_default(PanicOnJobDisarmEvent, || {
		catch_unwind(AssertUnwindSafe(|| command.spawn()))
	})
	.expect_err("the final-owner tracing callback must panic");
	assert_eq!(
		*panic.downcast::<&'static str>().unwrap(),
		"injected final-owner tracing panic"
	);

	let process = guard
		.lock()
		.unwrap()
		.take()
		.expect("the observing hook opened the descendant process");
	wait_for_process_exit(process)?;
	fs::remove_file(pid_file)?;
	Ok(())
}

#[test]
fn suspended_native_children_are_killed_after_later_failures() -> Result<()> {
	for failure in [Failure::Error, Failure::Panic] {
		for fail_before_job in [false, true] {
			let pid = Arc::new(AtomicU32::new(0));
			let mut command = CommandWrap::with_new("cmd.exe", |command| {
				command.args(["/D", "/S", "/C", "ping -n 30 127.0.0.1 >NUL"]);
			});
			command.wrap(CapturePid(Arc::clone(&pid)));
			let fail = FailWrapOnce {
				failure,
				failed: false,
			};
			if fail_before_job {
				command.wrap(fail).wrap(JobObject);
			} else {
				command.wrap(JobObject).wrap(fail);
			}

			match failure {
				Failure::Error => {
					let error = command.spawn().expect_err("the first child wrap must fail");
					assert_eq!(error.to_string(), "child wrapping failed");
				}
				Failure::Panic => {
					let panic = catch_unwind(AssertUnwindSafe(|| command.spawn()))
						.expect_err("the first child wrap must panic");
					assert_eq!(
						*panic.downcast::<&'static str>().unwrap(),
						"child wrapping failed"
					);
				}
			}

			let failed_pid = pid.load(Ordering::SeqCst);
			assert_ne!(failed_pid, 0);
			assert!(process_has_suspended_thread(failed_pid).is_err());

			let mut child = command.spawn()?;
			child.start_kill()?;
			let _ = wait_for_exit(child.as_mut())?;
		}
	}
	Ok(())
}

#[test]
fn preserves_flags_and_resumes_in_both_orders() -> Result<()> {
	let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
	for order in [Order::CreationFlagsFirst, Order::JobObjectFirst] {
		let mut command = command(flags, order);
		assert_eq!(command.get_wrap::<CreationFlags>().unwrap().0, flags);
		let mut child = command.spawn_with(|command| {
			let mut child = command.spawn()?;
			let suspended = match process_has_suspended_thread(child.id()) {
				Ok(suspended) => suspended,
				Err(error) => {
					let _ = child.kill();
					return Err(error);
				}
			};
			if suspended {
				Ok(child)
			} else {
				let _ = child.kill();
				Err(Error::other("child was not created suspended"))
			}
		})?;
		assert_eq!(command.get_wrap::<CreationFlags>().unwrap().0, flags);
		assert!(wait_for_exit(&mut *child)?.success());
	}
	Ok(())
}

#[test]
fn leaves_explicit_suspension_in_both_orders() -> Result<()> {
	let flags = CREATE_NO_WINDOW | CREATE_SUSPENDED;
	for order in [Order::CreationFlagsFirst, Order::JobObjectFirst] {
		let mut command = command(flags, order);
		let mut child = command.spawn()?;
		let remained_suspended = match process_has_suspended_thread(child.id()) {
			Ok(suspended) => suspended,
			Err(error) => {
				let _ = child.start_kill();
				return Err(error);
			}
		};
		child.start_kill()?;
		let _ = wait_for_exit(&mut *child)?;
		assert!(remained_suspended);
		assert_eq!(command.get_wrap::<CreationFlags>().unwrap().0, flags);
	}
	Ok(())
}
