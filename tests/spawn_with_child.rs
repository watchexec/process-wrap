#![cfg(all(any(feature = "std", feature = "tokio1"), any(unix, windows)))]

#[path = "support/bounded_process.rs"]
mod bounded_process;

macro_rules! spawn_with_child_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$child_wrapper_layer:path,
		$child_wrapper_slots:path,
		$pending_child_wrapper:path,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				any::TypeId,
				io,
				panic::{AssertUnwindSafe, catch_unwind, panic_any},
				sync::{
					Arc, Mutex,
					atomic::{AtomicUsize, Ordering},
				},
				thread::sleep,
				time::{Duration, Instant},
			};

			use super::bounded_process;
			use $child_wrapper as ChildWrapper;
			use $child_wrapper_layer as ChildWrapperLayer;
			use $child_wrapper_slots as ChildWrapperSlots;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $pending_child_wrapper as PendingChildWrapper;
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
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					self.0.lock().unwrap().push(Event::Wrap("first"));
					Ok(Some(PendingChildWrapper::new(FirstChild(None))))
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
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					self.0.lock().unwrap().push(Event::Wrap("second"));
					Ok(Some(PendingChildWrapper::new(SecondChild(None))))
				}
			}

			#[derive(Debug)]
			struct FirstChild(Option<Box<dyn ChildWrapper>>);

			impl ChildWrapperLayer for FirstChild {
				fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
					ChildWrapperSlots::new(&mut self.0)
				}
			}

			impl ChildWrapper for FirstChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self.0.as_deref().expect("the first layer is installed")
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.0.as_deref_mut().expect("the first layer is installed")
				}

				fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.0.take().expect("the first layer is installed")
				}

				#[cfg(windows)]
				fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
					self.inner().process_handle()
				}
			}

			#[derive(Debug)]
			struct SecondChild(Option<Box<dyn ChildWrapper>>);

			impl ChildWrapperLayer for SecondChild {
				fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
					ChildWrapperSlots::new(&mut self.0)
				}
			}

			impl ChildWrapper for SecondChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self.0.as_deref().expect("the second layer is installed")
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.0
						.as_deref_mut()
						.expect("the second layer is installed")
				}

				fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.0.take().expect("the second layer is installed")
				}

				#[cfg(windows)]
				fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
					self.inner().process_handle()
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

			#[derive(Debug)]
			struct SecondaryPayload(Arc<AtomicUsize>);

			impl Drop for SecondaryPayload {
				fn drop(&mut self) {
					self.0.fetch_add(1, Ordering::SeqCst);
					panic_any("a secondary wrapper payload was dropped");
				}
			}

			#[derive(Debug)]
			struct PanickingLayer {
				inner: Option<Box<dyn ChildWrapper>>,
				payload: Option<SecondaryPayload>,
			}

			impl ChildWrapperLayer for PanickingLayer {
				fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
					ChildWrapperSlots::new(&mut self.inner)
				}
			}

			impl ChildWrapper for PanickingLayer {
				fn inner(&self) -> &dyn ChildWrapper {
					self.inner
						.as_deref()
						.expect("the panicking layer is installed")
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.inner
						.as_deref_mut()
						.expect("the panicking layer is installed")
				}

				fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
					if let Some(payload) = self.payload.take() {
						std::mem::forget(payload);
					}
					self.inner.take().expect("the panicking layer is installed")
				}
			}

			impl Drop for PanickingLayer {
				fn drop(&mut self) {
					if let Some(payload) = self.payload.take() {
						panic_any(payload);
					}
				}
			}

			#[derive(Debug)]
			struct AddPanickingLayer(Mutex<Option<SecondaryPayload>>);

			impl CommandWrapper for AddPanickingLayer {
				fn wrap_child(
					&mut self,
					_child: &mut dyn ChildWrapper,
					_command: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					Ok(self.0.lock().unwrap().take().map(|payload| {
						PendingChildWrapper::new(PanickingLayer {
							inner: None,
							payload: Some(payload),
						})
					}))
				}
			}

			#[derive(Debug)]
			struct WrapIdentityError(Arc<()>);

			impl std::fmt::Display for WrapIdentityError {
				fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
					formatter.write_str("primary wrap error")
				}
			}

			impl std::error::Error for WrapIdentityError {}

			#[derive(Debug)]
			struct WrapIdentityPanic(Arc<()>);

			#[derive(Debug)]
			enum WrapFailure {
				Error(Arc<()>),
				Panic(Arc<()>),
			}

			#[derive(Debug)]
			struct FailWrap(Mutex<Option<WrapFailure>>);

			impl CommandWrapper for FailWrap {
				fn wrap_child(
					&mut self,
					_child: &mut dyn ChildWrapper,
					_command: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					let failure = self
						.0
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.take();
					match failure {
						Some(WrapFailure::Error(identity)) => {
							Err(io::Error::other(WrapIdentityError(identity)))
						}
						Some(WrapFailure::Panic(identity)) => {
							panic_any(WrapIdentityPanic(identity));
						}
						None => Ok(None),
					}
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
					_child: &mut dyn ChildWrapper,
					_core: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					self.visit(Phase::Wrap)?;
					Ok(None)
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

			fn bounded_test_process(
				test_name: &str,
				environment: (&str, &str),
				timeout: Duration,
			) -> (std::process::Output, bool) {
				let mut command = std::process::Command::new(std::env::current_exe().unwrap());
				command
					.args(["--exact", test_name, "--nocapture"])
					.env(environment.0, environment.1);
				bounded_process::run(command, timeout, None)
					.expect("run isolated wrapping regression")
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

			#[derive(Clone, Copy, Debug)]
			enum Transport {
				Native,
				ExplicitNative,
				Boxed,
			}

			fn spawn_transport(
				command: &mut CommandWrap,
				transport: Transport,
			) -> io::Result<Box<dyn ChildWrapper>> {
				match transport {
					Transport::Native => command.spawn(),
					Transport::ExplicitNative => command.spawn_with(|command| command.spawn()),
					Transport::Boxed => command
						.spawn_with_child(|_| Ok(Box::new(CustomLeaf) as Box<dyn ChildWrapper>)),
				}
			}

			fn assert_wrap_error(
				outcome: std::thread::Result<io::Result<Box<dyn ChildWrapper>>>,
				identity: &Arc<()>,
			) {
				let result = match outcome {
					Ok(result) => result,
					Err(secondary) => {
						std::mem::forget(secondary);
						panic!("child cleanup replaced the primary wrapping error");
					}
				};
				let error = result.expect_err("the wrapping hook must fail");
				let identity_error = error
					.get_ref()
					.and_then(|error| error.downcast_ref::<WrapIdentityError>())
					.expect("the exact wrapping error must survive cleanup");
				assert!(Arc::ptr_eq(&identity_error.0, identity));
			}

			fn assert_wrap_panic(
				outcome: std::thread::Result<io::Result<Box<dyn ChildWrapper>>>,
				identity: &Arc<()>,
			) {
				let payload = match outcome {
					Err(payload) => payload,
					Ok(_) => panic!("the wrapping hook must panic"),
				};
				let payload = match payload.downcast::<WrapIdentityPanic>() {
					Ok(payload) => payload,
					Err(secondary) => {
						std::mem::forget(secondary);
						panic!("child cleanup replaced the primary wrapping panic");
					}
				};
				assert!(Arc::ptr_eq(&payload.0, identity));
			}

			#[test]
			fn consuming_wrap_errors_preserve_identity_and_cleanup_for_every_transport() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for transport in [
					Transport::Native,
					Transport::ExplicitNative,
					Transport::Boxed,
				] {
					let secondary_drops = Arc::new(AtomicUsize::new(0));
					let identity = Arc::new(());
					let mut command = command();
					let payload = SecondaryPayload(Arc::clone(&secondary_drops));
					let failure = WrapFailure::Error(Arc::clone(&identity));
					command
						.wrap(AddPanickingLayer(Mutex::new(Some(payload))))
						.wrap(FailWrap(Mutex::new(Some(failure))));

					let outcome = catch_unwind(AssertUnwindSafe(|| {
						spawn_transport(&mut command, transport)
					}));
					assert_wrap_error(outcome, &identity);
					assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);

					let child = spawn_transport(&mut command, transport)
						.expect("the command and wrapping hooks remain reusable");
					if !matches!(transport, Transport::Boxed) {
						wait_for_exit(child);
					}
				}
			}

			#[test]
			fn consuming_wrap_panics_preserve_identity_and_cleanup_for_every_transport() {
				const CHILD_ENV: &str = "PROCESS_WRAP_CONSUMING_WRAP_PANIC";
				let module = stringify!($module);
				let selected = std::env::var(CHILD_ENV).ok();
				if selected
					.as_deref()
					.is_none_or(|value| !value.starts_with(module))
				{
					for transport in ["native", "explicit-native", "boxed"] {
						let child_value = format!("{module}:{transport}");
						let (output, timed_out) = bounded_test_process(
							concat!(
								stringify!($module),
								"::consuming_wrap_panics_preserve_identity_and_cleanup_for_every_transport"
							),
							(CHILD_ENV, &child_value),
							EXIT_TIMEOUT,
						);
						assert!(
							!timed_out,
							"isolated wrapping cleanup exceeded its deadline"
						);
						assert!(
							output.status.success(),
							"{transport} wrapping panic was not preserved:\nstdout:\n{}\nstderr:\n{}",
							String::from_utf8_lossy(&output.stdout),
							String::from_utf8_lossy(&output.stderr),
						);
					}
					return;
				}

				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let selected = selected.expect("the subprocess selected a transport");
				let transport = if selected.ends_with(":native") {
					Transport::Native
				} else if selected.ends_with(":explicit-native") {
					Transport::ExplicitNative
				} else {
					Transport::Boxed
				};
				let secondary_drops = Arc::new(AtomicUsize::new(0));
				let identity = Arc::new(());
				let mut command = command();
				let payload = SecondaryPayload(Arc::clone(&secondary_drops));
				let failure = WrapFailure::Panic(Arc::clone(&identity));
				command
					.wrap(AddPanickingLayer(Mutex::new(Some(payload))))
					.wrap(FailWrap(Mutex::new(Some(failure))));

				let outcome = catch_unwind(AssertUnwindSafe(|| {
					spawn_transport(&mut command, transport)
				}));
				assert_wrap_panic(outcome, &identity);
				assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
				let child = spawn_transport(&mut command, transport)
					.expect("the command and wrapping hooks remain reusable");
				if !matches!(transport, Transport::Boxed) {
					wait_for_exit(child);
				}
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
	process_wrap::std::ChildWrapperLayer,
	process_wrap::std::ChildWrapperSlots,
	process_wrap::std::PendingChildWrapper,
	None
);

#[cfg(feature = "tokio1")]
spawn_with_child_tests!(
	tokio_frontend,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	process_wrap::tokio::ChildWrapperLayer,
	process_wrap::tokio::ChildWrapperSlots,
	process_wrap::tokio::PendingChildWrapper,
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);
