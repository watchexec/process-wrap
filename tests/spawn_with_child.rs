#![cfg(all(any(feature = "std", feature = "tokio1"), any(unix, windows)))]

macro_rules! spawn_with_child_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				any::TypeId,
				io,
				panic::{AssertUnwindSafe, catch_unwind},
				sync::{Arc, Mutex},
				thread::sleep,
				time::{Duration, Instant},
			};

			use $child_wrapper as ChildWrapper;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $spawn_attempt as SpawnAttempt;

			const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Event {
				Pre(&'static str),
				Spawn,
				Post(&'static str),
				Wrap(&'static str),
			}

			#[derive(Debug)]
			struct First(Arc<Mutex<Vec<Event>>>);

			impl CommandWrapper for First {
				fn pre_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_core: &CommandWrap,
				) -> io::Result<()> {
					self.0.lock().unwrap().push(Event::Pre("first"));
					Ok(())
				}

				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<()> {
					self.0.lock().unwrap().push(Event::Post("first"));
					Ok(())
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					_core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.0.lock().unwrap().push(Event::Wrap("first"));
					Ok(Box::new(FirstChild(child)))
				}
			}

			#[derive(Debug)]
			struct Second(Arc<Mutex<Vec<Event>>>);

			impl CommandWrapper for Second {
				fn pre_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_core: &CommandWrap,
				) -> io::Result<()> {
					self.0.lock().unwrap().push(Event::Pre("second"));
					Ok(())
				}

				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<()> {
					self.0.lock().unwrap().push(Event::Post("second"));
					Ok(())
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					_core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.0.lock().unwrap().push(Event::Wrap("second"));
					Ok(Box::new(SecondChild(child)))
				}
			}

			#[derive(Debug)]
			struct FirstChild(Box<dyn ChildWrapper>);

			impl ChildWrapper for FirstChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self.0.as_ref()
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.0.as_mut()
				}

				fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.0
				}

				#[cfg(windows)]
				fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
					self.0.process_handle()
				}
			}

			#[derive(Debug)]
			struct SecondChild(Box<dyn ChildWrapper>);

			impl ChildWrapper for SecondChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self.0.as_ref()
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.0.as_mut()
				}

				fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.0
				}

				#[cfg(windows)]
				fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
					self.0.process_handle()
				}
			}

			#[derive(Debug)]
			struct InspectCompletedAttempt {
				portable: bool,
			}

			impl CommandWrapper for InspectCompletedAttempt {
				fn post_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<()> {
					assert_eq!(!attempt.is_native_only(), self.portable);
					assert_eq!(attempt.get_portable_args().is_some(), self.portable);
					assert_eq!(attempt.inherits_environment().is_some(), self.portable);
					Ok(())
				}
			}

			#[derive(Debug)]
			struct CustomLeaf;

			impl ChildWrapper for CustomLeaf {
				fn inner(&self) -> &dyn ChildWrapper {
					self
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self
				}

				fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
					self
				}
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Phase {
				Pre,
				Post,
				Wrap,
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Failure {
				Error,
				Panic,
			}

			#[derive(Debug)]
			struct FailOnce {
				phase: Phase,
				failure: Failure,
				failed: bool,
			}

			impl FailOnce {
				fn visit(&mut self, phase: Phase) -> io::Result<()> {
					if self.phase != phase || self.failed {
						return Ok(());
					}

					self.failed = true;
					match self.failure {
						Failure::Error => Err(io::Error::other("fail once")),
						Failure::Panic => panic!("fail once"),
					}
				}
			}

			impl CommandWrapper for FailOnce {
				fn pre_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_core: &CommandWrap,
				) -> io::Result<()> {
					self.visit(Phase::Pre)
				}

				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<()> {
					self.visit(Phase::Post)
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					_core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.visit(Phase::Wrap)?;
					Ok(child)
				}
			}

			fn runtime() -> Option<tokio::runtime::Runtime> {
				$runtime
			}

			fn command() -> CommandWrap {
				#[cfg(unix)]
				return CommandWrap::with_new("sh", |command| {
					command.args(["-c", "exit 0"]);
				});

				#[cfg(windows)]
				return CommandWrap::with_new("cmd.exe", |command| {
					command.args(["/D", "/S", "/C", "exit /b 0"]);
				});
			}

			fn wait_for_exit(mut child: Box<dyn ChildWrapper>) {
				let deadline = Instant::now() + EXIT_TIMEOUT;
				loop {
					if child.try_wait().unwrap().is_some() {
						return;
					}
					assert!(
						Instant::now() < deadline,
						"child did not exit before timeout"
					);
					sleep(Duration::from_millis(10));
				}
			}

			fn recover_hook(failure: Failure, phase: Phase) {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command();
				command.wrap(FailOnce {
					phase,
					failure,
					failed: false,
				});

				let mut spawn = || {
					command.spawn_with_child(|_| Ok(Box::new(CustomLeaf) as Box<dyn ChildWrapper>))
				};
				match failure {
					Failure::Error => assert_eq!(
						spawn().expect_err("the first hook must fail").to_string(),
						"fail once"
					),
					Failure::Panic => assert!(catch_unwind(AssertUnwindSafe(spawn)).is_err()),
				}

				assert!(command.get_wrap::<FailOnce>().unwrap().failed);
				let child = command
					.spawn_with_child(|command| {
						command
							.spawn()
							.map(|child| Box::new(child) as Box<dyn ChildWrapper>)
					})
					.expect("the restored command and wrapper must be reusable");
				wait_for_exit(child);
			}

			fn recover_spawner(failure: Failure) {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command();

				match failure {
					Failure::Error => {
						let error = command
							.spawn_with_child(|_| Err(io::Error::other("spawner failed")))
							.expect_err("the spawner must fail");
						assert_eq!(error.to_string(), "spawner failed");
					}
					Failure::Panic => {
						let panic = catch_unwind(AssertUnwindSafe(|| {
							let _ = command.spawn_with_child(
								|_| -> io::Result<Box<dyn ChildWrapper>> {
									panic!("spawner failed")
								},
							);
						}));
						assert!(panic.is_err());
					}
				}

				let child = command
					.spawn_with_child(|command| {
						command
							.spawn()
							.map(|child| Box::new(child) as Box<dyn ChildWrapper>)
					})
					.expect("the restored command must be reusable");
				wait_for_exit(child);
			}

			#[test]
			fn ordinary_spawn_keeps_portable_state_for_post_spawn_hooks() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command();
				command.wrap(InspectCompletedAttempt { portable: true });

				let child = command.spawn().expect("spawn native child");
				wait_for_exit(child);
			}

			#[test]
			fn boxed_child_runs_the_complete_wrapper_lifecycle() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let events = Arc::new(Mutex::new(Vec::new()));
				let mut command = command();
				command
					.wrap(First(Arc::clone(&events)))
					.wrap(Second(Arc::clone(&events)));

				let child = command
					.spawn_with_child(|_| {
						events.lock().unwrap().push(Event::Spawn);
						Ok(Box::new(CustomLeaf) as Box<dyn ChildWrapper>)
					})
					.expect("spawn custom child");

				assert_eq!(child.as_ref().type_id(), TypeId::of::<SecondChild>());
				assert_eq!(child.inner().type_id(), TypeId::of::<FirstChild>());
				assert_eq!(child.inner().inner().type_id(), TypeId::of::<CustomLeaf>());
				assert_eq!(
					*events.lock().unwrap(),
					vec![
						Event::Pre("first"),
						Event::Pre("second"),
						Event::Spawn,
						Event::Post("first"),
						Event::Post("second"),
						Event::Wrap("first"),
						Event::Wrap("second"),
					]
				);
			}

			#[test]
			fn native_spawn_with_still_runs_post_spawn() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let events = Arc::new(Mutex::new(Vec::new()));
				let mut command = command();
				command
					.wrap(First(Arc::clone(&events)))
					.wrap(Second(Arc::clone(&events)))
					.wrap(InspectCompletedAttempt { portable: false });

				let child = command
					.spawn_with(|command| {
						events.lock().unwrap().push(Event::Spawn);
						command.spawn()
					})
					.expect("spawn native child");
				wait_for_exit(child);

				assert_eq!(
					*events.lock().unwrap(),
					vec![
						Event::Pre("first"),
						Event::Pre("second"),
						Event::Spawn,
						Event::Post("first"),
						Event::Post("second"),
						Event::Wrap("first"),
						Event::Wrap("second"),
					]
				);
			}

			#[test]
			fn boxed_child_restores_hooks_after_errors() {
				for phase in [Phase::Pre, Phase::Post, Phase::Wrap] {
					recover_hook(Failure::Error, phase);
				}
			}

			#[test]
			fn boxed_child_restores_hooks_after_panics() {
				for phase in [Phase::Pre, Phase::Post, Phase::Wrap] {
					recover_hook(Failure::Panic, phase);
				}
			}

			#[test]
			fn boxed_child_restores_command_after_spawner_error() {
				recover_spawner(Failure::Error);
			}

			#[test]
			fn boxed_child_restores_command_after_spawner_panic() {
				recover_spawner(Failure::Panic);
			}
		}
	};
}

#[cfg(feature = "std")]
spawn_with_child_tests!(
	std_frontend,
	process_wrap::std::CommandWrap,
	process_wrap::std::SpawnAttempt,
	process_wrap::std::CommandWrapper,
	process_wrap::std::ChildWrapper,
	None
);

#[cfg(feature = "tokio1")]
spawn_with_child_tests!(
	tokio_frontend,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);
