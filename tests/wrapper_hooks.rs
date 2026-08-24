#![cfg(all(any(feature = "std", feature = "tokio1"), any(unix, windows)))]

macro_rules! wrapper_hook_tests {
	(
		$module:ident,
		$command:path,
		$child:path,
		$command_wrap:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				io,
				panic::{AssertUnwindSafe, catch_unwind},
				sync::{Arc, Mutex},
				thread::sleep,
				time::{Duration, Instant},
			};

			use $child as Child;
			use $child_wrapper as ChildWrapper;
			use $command as Command;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;

			const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Phase {
				Pre,
				Post,
				Wrap,
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum VisibilityEvent {
				First(Phase),
				Second(Phase),
				Spawn,
			}

			#[derive(Debug)]
			struct First(Arc<Mutex<Vec<VisibilityEvent>>>);

			impl First {
				fn observe(&self, phase: Phase, core: &CommandWrap) {
					assert!(core.has_wrap::<Self>());
					assert!(core.get_wrap::<Self>().is_none());
					assert!(core.has_wrap::<Second>());
					assert!(core.get_wrap::<Second>().is_some());
					self.0.lock().unwrap().push(VisibilityEvent::First(phase));
				}
			}

			impl CommandWrapper for First {
				fn pre_spawn(
					&mut self,
					_command: &mut Command,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.observe(Phase::Pre, core);
					Ok(())
				}

				fn post_spawn(
					&mut self,
					_command: &mut Command,
					_child: &mut Child,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.observe(Phase::Post, core);
					Ok(())
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.observe(Phase::Wrap, core);
					Ok(child)
				}
			}

			#[derive(Debug)]
			struct Second(Arc<Mutex<Vec<VisibilityEvent>>>);

			impl Second {
				fn observe(&self, phase: Phase, core: &CommandWrap) {
					assert!(core.has_wrap::<Self>());
					assert!(core.get_wrap::<Self>().is_none());
					assert!(core.has_wrap::<First>());
					assert!(core.get_wrap::<First>().is_some());
					self.0.lock().unwrap().push(VisibilityEvent::Second(phase));
				}
			}

			impl CommandWrapper for Second {
				fn pre_spawn(
					&mut self,
					_command: &mut Command,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.observe(Phase::Pre, core);
					Ok(())
				}

				fn post_spawn(
					&mut self,
					_command: &mut Command,
					_child: &mut Child,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.observe(Phase::Post, core);
					Ok(())
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.observe(Phase::Wrap, core);
					Ok(child)
				}
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum RecoveryEvent {
				Failing(Phase),
				Peer(Phase),
				Spawn,
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
				events: Arc<Mutex<Vec<RecoveryEvent>>>,
			}

			impl FailOnce {
				fn visit(&mut self, phase: Phase, core: &CommandWrap) -> io::Result<()> {
					assert!(core.has_wrap::<Self>());
					assert!(core.get_wrap::<Self>().is_none());
					assert!(core.has_wrap::<Peer>());
					assert!(core.get_wrap::<Peer>().is_some());
					self.events
						.lock()
						.unwrap()
						.push(RecoveryEvent::Failing(phase));

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
					_command: &mut Command,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.visit(Phase::Pre, core)
				}

				fn post_spawn(
					&mut self,
					_command: &mut Command,
					_child: &mut Child,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.visit(Phase::Post, core)
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.visit(Phase::Wrap, core)?;
					Ok(child)
				}
			}

			#[derive(Debug)]
			struct Peer(Arc<Mutex<Vec<RecoveryEvent>>>);

			impl Peer {
				fn observe(&self, phase: Phase, core: &CommandWrap) {
					assert!(core.has_wrap::<Self>());
					assert!(core.get_wrap::<Self>().is_none());
					assert!(core.has_wrap::<FailOnce>());
					assert!(core.get_wrap::<FailOnce>().is_some());
					self.0.lock().unwrap().push(RecoveryEvent::Peer(phase));
				}
			}

			impl CommandWrapper for Peer {
				fn pre_spawn(
					&mut self,
					_command: &mut Command,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.observe(Phase::Pre, core);
					Ok(())
				}

				fn post_spawn(
					&mut self,
					_command: &mut Command,
					_child: &mut Child,
					core: &CommandWrap,
				) -> io::Result<()> {
					self.observe(Phase::Post, core);
					Ok(())
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					core: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.observe(Phase::Wrap, core);
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

			fn retry_events() -> Vec<RecoveryEvent> {
				vec![
					RecoveryEvent::Failing(Phase::Pre),
					RecoveryEvent::Peer(Phase::Pre),
					RecoveryEvent::Spawn,
					RecoveryEvent::Failing(Phase::Post),
					RecoveryEvent::Peer(Phase::Post),
					RecoveryEvent::Failing(Phase::Wrap),
					RecoveryEvent::Peer(Phase::Wrap),
				]
			}

			fn recover_after(failure: Failure, phase: Phase) {
				let events = Arc::new(Mutex::new(Vec::new()));
				let mut command = command();
				command
					.wrap(FailOnce {
						phase,
						failure,
						failed: false,
						events: Arc::clone(&events),
					})
					.wrap(Peer(Arc::clone(&events)));

				let mut spawn = || {
					command.spawn_with(|command| {
						events.lock().unwrap().push(RecoveryEvent::Spawn);
						command.spawn()
					})
				};

				match failure {
					Failure::Error => assert_eq!(
						spawn()
							.expect_err("FailOnce is guaranteed to fail its first hook invocation")
							.to_string(),
						"fail once"
					),
					Failure::Panic => assert!(catch_unwind(AssertUnwindSafe(spawn)).is_err()),
				}

				assert!(command.has_wrap::<FailOnce>());
				assert!(command.get_wrap::<FailOnce>().unwrap().failed);
				assert!(command.get_wrap::<Peer>().is_some());

				events.lock().unwrap().clear();
				let child = command
					.spawn_with(|command| {
						events.lock().unwrap().push(RecoveryEvent::Spawn);
						command.spawn()
					})
					.expect(
						"state restoration guarantees that the command and wrappers remain reusable",
					);
				wait_for_exit(child);
				assert_eq!(*events.lock().unwrap(), retry_events());
			}

			#[test]
			fn peers_are_visible_and_hooks_keep_registration_order() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let events = Arc::new(Mutex::new(Vec::new()));
				let mut command = command();
				command
					.wrap(First(Arc::clone(&events)))
					.wrap(Second(Arc::clone(&events)));

				let child = command
					.spawn_with(|command| {
						events.lock().unwrap().push(VisibilityEvent::Spawn);
						command.spawn()
					})
					.unwrap();
				wait_for_exit(child);

				assert_eq!(
					*events.lock().unwrap(),
					vec![
						VisibilityEvent::First(Phase::Pre),
						VisibilityEvent::Second(Phase::Pre),
						VisibilityEvent::Spawn,
						VisibilityEvent::First(Phase::Post),
						VisibilityEvent::Second(Phase::Post),
						VisibilityEvent::First(Phase::Wrap),
						VisibilityEvent::Second(Phase::Wrap),
					]
				);
				assert!(command.get_wrap::<First>().is_some());
				assert!(command.get_wrap::<Second>().is_some());
			}

			#[test]
			fn restores_each_hook_after_error() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for phase in [Phase::Pre, Phase::Post, Phase::Wrap] {
					recover_after(Failure::Error, phase);
				}
			}

			#[test]
			fn restores_each_hook_after_panic() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for phase in [Phase::Pre, Phase::Post, Phase::Wrap] {
					recover_after(Failure::Panic, phase);
				}
			}

			#[test]
			fn restores_command_after_spawner_error() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command();
				let error = command
					.spawn_with(|_| Err(io::Error::other("spawner failed")))
					.expect_err("the test spawner is guaranteed to fail");
				assert_eq!(error.to_string(), "spawner failed");
				wait_for_exit(
					command
						.spawn()
						.expect("state restoration guarantees that the command remains reusable"),
				);
			}

			#[test]
			fn restores_command_after_spawner_panic() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let mut command = command();
				let panic = catch_unwind(AssertUnwindSafe(|| {
					let _ = command.spawn_with(|_| -> io::Result<Child> {
						panic!("spawner failed");
					});
				}));
				assert!(panic.is_err());
				wait_for_exit(
					command
						.spawn()
						.expect("state restoration guarantees that the command remains reusable"),
				);
			}
		}
	};
}

#[cfg(feature = "std")]
wrapper_hook_tests!(
	std_frontend,
	std::process::Command,
	std::process::Child,
	process_wrap::std::CommandWrap,
	process_wrap::std::CommandWrapper,
	process_wrap::std::ChildWrapper,
	None
);

#[cfg(feature = "tokio1")]
wrapper_hook_tests!(
	tokio_frontend,
	tokio::process::Command,
	tokio::process::Child,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);
