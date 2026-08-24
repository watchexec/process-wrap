#![cfg(all(
	unix,
	any(feature = "std", feature = "tokio1"),
	feature = "process-group",
	feature = "process-session",
	feature = "reset-sigmask"
))]

macro_rules! unix_attempt_policy_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$process_group:path,
		$process_group_child:path,
		$process_group_target:path,
		$process_session:path,
		$reset_sigmask:path,
		$replace_native:expr,
		$child_id:expr,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				any::Any,
				io,
				os::unix::process::CommandExt,
				panic::{AssertUnwindSafe, catch_unwind, panic_any},
				process::{Command as NativeCommand, ExitStatus, Stdio},
				thread::sleep,
				time::{Duration, Instant},
			};

			use nix::{
				sys::signal::{Signal, killpg},
				unistd::{Pid, getpgid},
			};
			use $child_wrapper as ChildWrapper;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $process_group as ProcessGroup;
			use $process_group_child as ProcessGroupChild;
			use $process_group_target as ProcessGroupTarget;
			use $process_session as ProcessSession;
			use $reset_sigmask as ResetSigmask;
			use $spawn_attempt as SpawnAttempt;

			const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

			#[derive(Clone, Copy, Debug)]
			enum Failure {
				Error,
				Panic,
			}

			#[derive(Debug)]
			struct FailAfterNativeAccess {
				failure: Failure,
				failed: bool,
			}

			impl CommandWrapper for FailAfterNativeAccess {
				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<()> {
					if self.failed {
						return Ok(());
					}

					let _ = attempt.native_mut();
					self.failed = true;
					match self.failure {
						Failure::Error => Err(io::Error::other("fail after native access")),
						Failure::Panic => panic_any("fail after native access"),
					}
				}
			}

			#[derive(Debug)]
			struct AccessNative;

			impl CommandWrapper for AccessNative {
				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<()> {
					let _ = attempt.native_mut();
					Ok(())
				}
			}

			#[derive(Debug)]
			struct InspectPortable;

			impl CommandWrapper for InspectPortable {
				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<()> {
					assert!(!attempt.is_native_only());
					assert_eq!(attempt.process_group_target(), None);
					assert!(attempt.creates_process_session());
					assert!(attempt.resets_sigmask());
					Ok(())
				}
			}

			#[derive(Debug)]
			struct InspectGroup(ProcessGroupTarget);

			impl CommandWrapper for InspectGroup {
				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<()> {
					assert_eq!(attempt.process_group_target(), Some(self.0));
					assert!(!attempt.creates_process_session());
					assert!(!attempt.resets_sigmask());
					Ok(())
				}
			}

			#[derive(Debug)]
			struct ReplaceNative;

			impl CommandWrapper for ReplaceNative {
				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<()> {
					($replace_native)(attempt.native_mut());
					Ok(())
				}
			}

			#[derive(Debug)]
			struct ExternalGroup {
				child: std::process::Child,
				pgid: Pid,
			}

			impl ExternalGroup {
				fn spawn() -> Self {
					let mut command = NativeCommand::new("sh");
					command
						.args(["-c", "sleep 30"])
						.stdin(Stdio::null())
						.stdout(Stdio::null())
						.stderr(Stdio::null())
						.process_group(0);
					let child = command.spawn().unwrap();
					let pgid = Pid::from_raw(i32::try_from(child.id()).unwrap());
					Self { child, pgid }
				}

				fn kill(&self) {
					let _ = killpg(self.pgid, Signal::SIGKILL);
				}
			}

			impl Drop for ExternalGroup {
				fn drop(&mut self) {
					self.kill();
					let _ = self.child.wait();
				}
			}

			fn runtime() -> Option<tokio::runtime::Runtime> {
				$runtime
			}

			fn fail<T>(failure: Failure, message: &'static str) -> io::Result<T> {
				match failure {
					Failure::Error => Err(io::Error::other(message)),
					Failure::Panic => panic_any(message),
				}
			}

			fn command_with_exit(code: i32) -> CommandWrap {
				CommandWrap::with_new("sh", |command| {
					command.args(["-c", &format!("exit {code}")]);
				})
			}

			fn child_id(child: &dyn ChildWrapper) -> u32 {
				($child_id)(child)
			}

			fn wait_for_exit(child: &mut dyn ChildWrapper) -> ExitStatus {
				let deadline = Instant::now() + EXIT_TIMEOUT;
				loop {
					if let Some(status) = child.try_wait().unwrap() {
						return status;
					}
					assert!(
						Instant::now() < deadline,
						"child did not exit before timeout"
					);
					sleep(Duration::from_millis(10));
				}
			}

			#[test]
			fn native_only_session_command_is_reusable() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command_with_exit(0);
				let _ = command.native_mut();
				command.wrap(ProcessSession);

				for _ in 0..3 {
					let mut child = command.spawn().unwrap();
					assert!(wait_for_exit(child.as_mut()).success());
				}
			}

			#[test]
			fn tracked_dispatcher_is_attached_once_per_attempt() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command_with_exit(0);
				command.wrap(ProcessSession).wrap(AccessNative);

				for _ in 0..3 {
					let mut child = command.spawn().unwrap();
					assert!(wait_for_exit(child.as_mut()).success());
				}
			}

			#[test]
			fn child_setup_survives_native_command_replacement() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command_with_exit(0);
				command.wrap(ProcessSession).wrap(ReplaceNative);

				let mut child = command.spawn().unwrap();
				child.start_kill().unwrap();
				let _ = wait_for_exit(child.as_mut());
			}

			#[test]
			fn native_only_dispatcher_recovers_after_errors_and_panics() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for failure in [Failure::Error, Failure::Panic] {
					let mut command = command_with_exit(0);
					let _ = command.native_mut();
					command.wrap(ProcessSession).wrap(FailAfterNativeAccess {
						failure,
						failed: false,
					});

					match failure {
						Failure::Error => assert_eq!(
							command.spawn().unwrap_err().to_string(),
							"fail after native access"
						),
						Failure::Panic => {
							let panic = catch_unwind(AssertUnwindSafe(|| command.spawn()))
								.expect_err("the first spawn must panic");
							assert_eq!(
								*panic.downcast::<&'static str>().unwrap(),
								"fail after native access"
							);
						}
					}

					for _ in 0..2 {
						let mut child = command.spawn().unwrap();
						assert!(wait_for_exit(child.as_mut()).success());
					}
				}
			}

			#[test]
			fn explicit_native_replacement_forces_dispatcher_reinstallation() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for boxed_child in [false, true] {
					for failure in [Failure::Error, Failure::Panic] {
						let mut command = command_with_exit(0);
						let _ = command.native_mut();
						command.wrap(ProcessGroup::leader());

						let outcome = catch_unwind(AssertUnwindSafe(|| {
							if boxed_child {
								command.spawn_with_child(|native| {
									($replace_native)(native);
									fail(failure, "explicit spawner failed")
								})
							} else {
								command.spawn_with(|native| {
									($replace_native)(native);
									fail(failure, "explicit spawner failed")
								})
							}
						}));
						match failure {
							Failure::Error => assert_eq!(
								outcome.unwrap().unwrap_err().to_string(),
								"explicit spawner failed"
							),
							Failure::Panic => assert_eq!(
								*outcome
									.expect_err("the explicit spawner must panic")
									.downcast::<&'static str>()
									.unwrap(),
								"explicit spawner failed"
							),
						}

						let mut child = command.spawn().unwrap();
						let pid = Pid::from_raw(i32::try_from(child_id(child.as_ref())).unwrap());
						assert_eq!(getpgid(Some(pid)).unwrap(), pid);
						child.start_kill().unwrap();
						let _ = wait_for_exit(child.as_mut());
					}
				}
			}

			#[test]
			fn built_in_policy_hooks_keep_the_attempt_portable() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command_with_exit(0);
				command
					.wrap(ProcessSession)
					.wrap(ResetSigmask)
					.wrap(InspectPortable);

				let mut child = command.spawn().unwrap();
				assert!(wait_for_exit(child.as_mut()).success());
			}

			#[test]
			fn process_group_policy_is_visible_without_native_state() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command_with_exit(0);
				command
					.wrap(ProcessGroup::leader())
					.wrap(InspectGroup(ProcessGroupTarget::Leader));

				let mut child = command.spawn().unwrap();
				assert!(wait_for_exit(child.as_mut()).success());
			}

			#[test]
			fn attach_to_tracks_the_direct_pid_and_actual_group() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let external = ExternalGroup::spawn();
				let pgid = u32::try_from(external.pgid.as_raw()).unwrap();
				let mut command = command_with_exit(7);
				command
					.wrap(ProcessGroup::attach_to(pgid))
					.wrap(InspectGroup(ProcessGroupTarget::AttachTo(pgid)));

				let mut child = command.spawn().unwrap();
				let direct_pid = child_id(child.as_ref());
				assert_ne!(direct_pid, pgid);
				let group_child = (child.as_ref() as &dyn Any)
					.downcast_ref::<ProcessGroupChild>()
					.expect("ProcessGroup installs its child layer");
				assert_eq!(group_child.pgid(), pgid);

				sleep(Duration::from_millis(200));
				assert_eq!(child.try_wait().unwrap(), None);
				external.kill();
				assert_eq!(wait_for_exit(child.as_mut()).code(), Some(7));
			}

			#[test]
			fn existing_group_and_new_session_conflict_in_either_order() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for session_first in [false, true] {
					let mut command = command_with_exit(0);
					if session_first {
						command
							.wrap(ProcessSession)
							.wrap(ProcessGroup::attach_to(1));
					} else {
						command
							.wrap(ProcessGroup::attach_to(1))
							.wrap(ProcessSession);
					}

					let error = command.spawn().unwrap_err();
					assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
					assert_eq!(
						error.to_string(),
						"a process cannot join an existing process group and create a new session"
					);
				}
			}

			#[test]
			fn invalid_existing_group_ids_are_rejected_before_spawn() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for (pgid, message) in [
					(0, "an existing process group ID must be positive"),
					(u32::MAX, "process group ID exceeds the platform range"),
				] {
					let mut command = command_with_exit(0);
					command.wrap(ProcessGroup::attach_to(pgid));
					let error = command.spawn().unwrap_err();
					assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
					assert_eq!(error.to_string(), message);
				}
			}
		}
	};
}

#[cfg(feature = "std")]
unix_attempt_policy_tests!(
	std_frontend,
	process_wrap::std::CommandWrap,
	process_wrap::std::SpawnAttempt,
	process_wrap::std::CommandWrapper,
	process_wrap::std::ChildWrapper,
	process_wrap::std::ProcessGroup,
	process_wrap::std::ProcessGroupChild,
	process_wrap::ProcessGroupTarget,
	process_wrap::std::ProcessSession,
	process_wrap::std::ResetSigmask,
	|command: &mut std::process::Command| {
		*command = std::process::Command::new("sh");
		command.args(["-c", "sleep 30"]);
	},
	|child: &dyn process_wrap::std::ChildWrapper| child.id(),
	None
);

#[cfg(feature = "tokio1")]
unix_attempt_policy_tests!(
	tokio_frontend,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	process_wrap::tokio::ProcessGroup,
	process_wrap::tokio::ProcessGroupChild,
	process_wrap::ProcessGroupTarget,
	process_wrap::tokio::ProcessSession,
	process_wrap::tokio::ResetSigmask,
	|command: &mut tokio::process::Command| {
		*command = tokio::process::Command::new("sh");
		command.args(["-c", "sleep 30"]);
	},
	|child: &dyn process_wrap::tokio::ChildWrapper| child.id().unwrap(),
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);
