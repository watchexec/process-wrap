use std::{
	io::{Error, ErrorKind},
	panic::{AssertUnwindSafe, catch_unwind},
	process::ExitStatus,
	sync::{
		Arc,
		atomic::{AtomicU32, Ordering},
	},
	time::Instant,
};

use windows::Win32::System::Threading::{
	CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_CREATION_FLAGS,
};

use super::{prelude::*, windows_thread::process_has_suspended_thread};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

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
