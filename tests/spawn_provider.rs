#![cfg(all(any(feature = "std", feature = "tokio1"), any(unix, windows)))]

macro_rules! spawn_provider_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$provider_product:path,
		$spawn_provider:path,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				any::TypeId,
				ffi::OsStr,
				io,
				panic::{AssertUnwindSafe, catch_unwind, panic_any},
				sync::{
					Arc, Mutex,
					atomic::{AtomicBool, Ordering},
				},
				thread::sleep,
				time::{Duration, Instant},
			};

			use process_wrap::{CommandArg, SpawnTransaction};
			use $child_wrapper as ChildWrapper;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $provider_product as ProviderProduct;
			use $spawn_attempt as SpawnAttempt;
			use $spawn_provider as SpawnProvider;

			const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Point {
				Available,
				ValidateCommand,
				Pre,
				ValidateAttempt,
				Spawn,
				Post,
				PeerPost,
				Wrap,
				PeerWrap,
				Commit,
				Rollback,
				#[cfg(windows)]
				FinalizeSpawn,
				#[cfg(windows)]
				DisarmSpawnCleanup,
				#[cfg(windows)]
				DisarmNonOwner,
				#[cfg(windows)]
				DisarmOwner,
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Failure {
				Error(io::ErrorKind, &'static str),
				Panic(&'static str),
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Event {
				Extend(&'static str, &'static str),
				Available(&'static str),
				ValidateCommand(&'static str),
				Pre(&'static str),
				ValidateAttempt(&'static str),
				Spawn(&'static str),
				Post(&'static str),
				Wrap(&'static str),
				Commit,
				Rollback,
				#[cfg(windows)]
				FinalizeSpawn(&'static str),
				#[cfg(windows)]
				DisarmSpawnCleanup(&'static str),
				#[cfg(windows)]
				DisarmNonOwner(&'static str),
				#[cfg(windows)]
				DisarmOwner(&'static str),
				#[cfg(windows)]
				OwnerDropped(&'static str, bool),
			}

			#[cfg(windows)]
			#[derive(Clone, Copy, Debug)]
			struct FinalizationLayerSpec {
				name: &'static str,
				owns_job_cleanup: bool,
			}

			#[derive(Debug, Default)]
			struct Shared {
				events: Mutex<Vec<Event>>,
				failures: Mutex<Vec<(Point, Failure)>>,
				make_attempt_native_only: AtomicBool,
				#[cfg(windows)]
				finalization_layers: Mutex<Vec<FinalizationLayerSpec>>,
			}

			impl Shared {
				fn event(&self, event: Event) {
					self.events.lock().unwrap().push(event);
				}

				fn events(&self) -> Vec<Event> {
					self.events.lock().unwrap().clone()
				}

				fn clear_events(&self) {
					self.events.lock().unwrap().clear();
				}

				#[cfg(windows)]
				fn set_finalization_layers(&self, layers: Vec<FinalizationLayerSpec>) {
					*self.finalization_layers.lock().unwrap() = layers;
				}

				#[cfg(windows)]
				fn finalization_layers(&self) -> Vec<FinalizationLayerSpec> {
					self.finalization_layers.lock().unwrap().clone()
				}

				fn fail_once(&self, point: Point, failure: Failure) {
					self.failures.lock().unwrap().push((point, failure));
				}

				fn fail(&self, point: Point) -> io::Result<()> {
					let failure = {
						let mut failures = self.failures.lock().unwrap();
						failures
							.iter()
							.position(|(candidate, _)| *candidate == point)
							.map(|index| failures.remove(index).1)
					};

					match failure {
						Some(Failure::Error(kind, message)) => Err(io::Error::new(kind, message)),
						Some(Failure::Panic(message)) => panic_any(message),
						None => Ok(()),
					}
				}
			}

			#[derive(Debug)]
			struct CustomChild;

			impl ChildWrapper for CustomChild {
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

			#[cfg(windows)]
			#[derive(Debug)]
			struct FinalizationChild {
				inner: Option<Box<dyn ChildWrapper>>,
				shared: Arc<Shared>,
				name: &'static str,
				owns_job_cleanup: bool,
				armed: bool,
			}

			#[cfg(windows)]
			impl ChildWrapper for FinalizationChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self.inner.as_deref().expect("the child layer still owns its inner child")
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.inner
						.as_deref_mut()
						.expect("the child layer still owns its inner child")
				}

				fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.inner
						.take()
						.expect("the child layer still owns its inner child")
				}

				fn finalize_spawn_layer(&mut self) -> io::Result<()> {
					self.shared.event(Event::FinalizeSpawn(self.name));
					self.shared.fail(Point::FinalizeSpawn)
				}

				fn disarm_spawn_cleanup_layer(&mut self) -> io::Result<()> {
					self.shared.event(Event::DisarmSpawnCleanup(self.name));
					self.shared.fail(Point::DisarmSpawnCleanup)
				}

				fn owns_job_object_cleanup_layer(&self) -> bool {
					self.owns_job_cleanup
				}

				fn disarm_job_object_layer(&mut self) -> io::Result<()> {
					if self.owns_job_cleanup {
						self.shared.event(Event::DisarmOwner(self.name));
						self.shared.fail(Point::DisarmOwner)?;
						self.armed = false;
					} else {
						self.shared.event(Event::DisarmNonOwner(self.name));
						self.shared.fail(Point::DisarmNonOwner)?;
					}
					Ok(())
				}
			}

			#[cfg(windows)]
			impl Drop for FinalizationChild {
				fn drop(&mut self) {
					if self.owns_job_cleanup {
						self.shared.event(Event::OwnerDropped(self.name, self.armed));
					}
				}
			}

			#[cfg(windows)]
			fn add_finalization_layers(
				shared: Arc<Shared>,
				child: Box<dyn ChildWrapper>,
			) -> Box<dyn ChildWrapper> {
				shared
					.finalization_layers()
					.into_iter()
					.rev()
					.fold(child, |inner, layer| {
						Box::new(FinalizationChild {
							inner: Some(inner),
							shared: Arc::clone(&shared),
							name: layer.name,
							owns_job_cleanup: layer.owns_job_cleanup,
							armed: layer.owns_job_cleanup,
						})
					})
			}

			#[derive(Debug)]
			struct Transaction(Arc<Shared>);

			impl SpawnTransaction for Transaction {
				fn commit(&mut self) -> io::Result<()> {
					self.0.event(Event::Commit);
					self.0.fail(Point::Commit)
				}

				fn rollback(&mut self) -> io::Result<()> {
					self.0.event(Event::Rollback);
					self.0.fail(Point::Rollback)
				}
			}

			#[derive(Debug)]
			struct Provider {
				name: &'static str,
				shared: Arc<Shared>,
			}

			impl Provider {
				fn assert_callback_visibility(&self, command: &CommandWrap) {
					assert!(command.has_wrap::<ProviderWrapper>());
					assert!(command.get_wrap::<ProviderWrapper>().is_none());
					assert!(command.has_wrap::<Peer>());
					assert!(command.get_wrap::<Peer>().is_some());
				}
			}

			impl SpawnProvider for Provider {
				fn check_available(&self) -> io::Result<()> {
					self.shared.event(Event::Available(self.name));
					self.shared.fail(Point::Available)
				}

				fn validate_command(&self, command: &CommandWrap) -> io::Result<()> {
					self.assert_callback_visibility(command);
					assert_eq!(command.inherits_environment(), Some(false));
					let args = command
						.get_portable_args()
						.expect("provider validation receives tracked portable arguments");
					assert!(matches!(args.first(), Some(CommandArg::Regular(_))));
					#[cfg(windows)]
					assert!(matches!(
						args.last(),
						Some(CommandArg::Raw(arg)) if arg == OsStr::new(" provider-raw-fragment")
					));
					#[cfg(unix)]
					assert!(args.iter().all(|arg| matches!(arg, CommandArg::Regular(_))));
					assert!(command.get_envs().any(|(key, value)| {
						key == OsStr::new("PROCESS_WRAP_PROVIDER_BASE")
							&& value == Some(OsStr::new("set"))
					}));
					assert!(
						!command
							.get_envs()
							.any(|(key, _)| key == OsStr::new("PROCESS_WRAP_PROVIDER_HOOK"))
					);
					self.shared.event(Event::ValidateCommand(self.name));
					self.shared.fail(Point::ValidateCommand)
				}

				fn validate_attempt(
					&self,
					attempt: &SpawnAttempt,
					command: &CommandWrap,
				) -> io::Result<()> {
					self.assert_callback_visibility(command);
					assert!(!attempt.is_native_only());
					#[cfg(unix)]
					{
						assert_eq!(attempt.process_group_target(), None);
						assert!(!attempt.creates_process_session());
						assert!(!attempt.resets_sigmask());
					}
					assert_eq!(attempt.inherits_environment(), Some(false));
					let args = attempt
						.get_portable_args()
						.expect("provider validation receives tracked portable arguments");
					assert!(matches!(args.first(), Some(CommandArg::Regular(_))));
					#[cfg(windows)]
					assert!(matches!(
						args.last(),
						Some(CommandArg::Raw(arg)) if arg == OsStr::new(" provider-raw-fragment")
					));
					#[cfg(unix)]
					assert!(args.iter().all(|arg| matches!(arg, CommandArg::Regular(_))));
					assert!(attempt.get_envs().any(|(key, value)| {
						key == OsStr::new("PROCESS_WRAP_PROVIDER_HOOK")
							&& value == Some(OsStr::new(self.name))
					}));
					assert!(attempt.get_envs().any(|(key, value)| {
						key == OsStr::new("PROCESS_WRAP_PROVIDER_PEER")
							&& value == Some(OsStr::new("set"))
					}));
					self.shared.event(Event::ValidateAttempt(self.name));
					self.shared.fail(Point::ValidateAttempt)
				}

				fn spawn(
					&self,
					attempt: &mut SpawnAttempt,
					command: &CommandWrap,
				) -> io::Result<ProviderProduct> {
					self.assert_callback_visibility(command);
					assert!(!attempt.is_native_only());
					self.shared.event(Event::Spawn(self.name));
					self.shared.fail(Point::Spawn)?;
					Ok(ProviderProduct::new(
						Box::new(CustomChild),
						Box::new(Transaction(Arc::clone(&self.shared))),
					))
				}
			}

			#[derive(Debug)]
			struct ProviderWrapper {
				name: &'static str,
				provider: Provider,
			}

			impl ProviderWrapper {
				fn new(name: &'static str, shared: Arc<Shared>) -> Self {
					Self {
						name,
						provider: Provider { name, shared },
					}
				}

				fn shared(&self) -> &Arc<Shared> {
					&self.provider.shared
				}

				fn assert_hook_visibility(&self, command: &CommandWrap) {
					assert!(command.has_wrap::<Self>());
					assert!(command.get_wrap::<Self>().is_none());
					assert!(command.has_wrap::<Peer>());
					assert!(command.get_wrap::<Peer>().is_some());
				}
			}

			impl CommandWrapper for ProviderWrapper {
				fn extend(&mut self, other: Self) {
					self.shared().event(Event::Extend(self.name, other.name));
					self.name = other.name;
					self.provider = other.provider;
				}

				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					command: &CommandWrap,
				) -> io::Result<()> {
					self.assert_hook_visibility(command);
					self.shared().event(Event::Pre(self.name));
					attempt.env("PROCESS_WRAP_PROVIDER_HOOK", self.name);
					if self
						.shared()
						.make_attempt_native_only
						.swap(false, Ordering::SeqCst)
					{
						let _ = attempt.native_mut();
					}
					self.shared().fail(Point::Pre)
				}

				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					child: &mut dyn ChildWrapper,
					command: &CommandWrap,
				) -> io::Result<()> {
					self.assert_hook_visibility(command);
					assert_eq!(child.type_id(), TypeId::of::<CustomChild>());
					self.shared().event(Event::Post(self.name));
					self.shared().fail(Point::Post)
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					command: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.assert_hook_visibility(command);
					assert_eq!(child.as_ref().type_id(), TypeId::of::<CustomChild>());
					self.shared().event(Event::Wrap(self.name));
					self.shared().fail(Point::Wrap)?;
					Ok(child)
				}

				fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
					Some(&self.provider)
				}
			}

			#[derive(Debug)]
			struct Peer {
				shared: Arc<Shared>,
				expect_provider: bool,
				expect_custom_child: bool,
			}

			impl Peer {
				fn assert_hook_visibility(&self, command: &CommandWrap) {
					assert!(command.has_wrap::<Self>());
					assert!(command.get_wrap::<Self>().is_none());
					if self.expect_provider {
						assert!(command.has_wrap::<ProviderWrapper>());
						assert!(command.get_wrap::<ProviderWrapper>().is_some());
					}
				}
			}

			impl CommandWrapper for Peer {
				fn pre_spawn(
					&mut self,
					attempt: &mut SpawnAttempt,
					command: &CommandWrap,
				) -> io::Result<()> {
					self.assert_hook_visibility(command);
					self.shared.event(Event::Pre("peer"));
					attempt.env("PROCESS_WRAP_PROVIDER_PEER", "set");
					Ok(())
				}

				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					child: &mut dyn ChildWrapper,
					command: &CommandWrap,
				) -> io::Result<()> {
					self.assert_hook_visibility(command);
					if self.expect_custom_child {
						assert_eq!(child.type_id(), TypeId::of::<CustomChild>());
					}
					self.shared.event(Event::Post("peer"));
					self.shared.fail(Point::PeerPost)
				}

				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					command: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					self.assert_hook_visibility(command);
					if self.expect_custom_child {
						assert_eq!(child.as_ref().type_id(), TypeId::of::<CustomChild>());
					}
					self.shared.event(Event::Wrap("peer"));
					self.shared.fail(Point::PeerWrap)?;
					#[cfg(windows)]
					let child = add_finalization_layers(Arc::clone(&self.shared), child);
					Ok(child)
				}
			}

			#[derive(Debug)]
			struct OtherProviderWrapper(Provider);

			impl CommandWrapper for OtherProviderWrapper {
				fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
					Some(&self.0)
				}
			}

			fn runtime() -> Option<tokio::runtime::Runtime> {
				$runtime
			}

			fn command() -> CommandWrap {
				#[cfg(unix)]
				let mut command = CommandWrap::with_new("sh", |command| {
					command.args(["-c", "exit 0"]);
				});

				#[cfg(windows)]
				let mut command = CommandWrap::with_new("cmd.exe", |command| {
					command.args(["/D", "/S", "/C", "exit /b 0"]);
				});

				command.env("PROCESS_WRAP_PROVIDER_BASE", "set");
				command
			}

			fn configure_provider_intent(command: &mut CommandWrap) {
				command
					.env_clear()
					.env("PROCESS_WRAP_PROVIDER_BASE", "set");
				#[cfg(windows)]
				command.raw_arg(" provider-raw-fragment");
			}

			fn provider_command(shared: Arc<Shared>, name: &'static str) -> CommandWrap {
				let mut command = command();
				configure_provider_intent(&mut command);
				command
					.wrap(ProviderWrapper::new(name, Arc::clone(&shared)))
					.wrap(Peer {
						shared,
						expect_provider: true,
						expect_custom_child: true,
					});
				command
			}

			fn successful_events(name: &'static str) -> Vec<Event> {
				vec![
					Event::Available(name),
					Event::ValidateCommand(name),
					Event::Pre(name),
					Event::Pre("peer"),
					Event::ValidateAttempt(name),
					Event::Spawn(name),
					Event::Post(name),
					Event::Post("peer"),
					Event::Wrap(name),
					Event::Wrap("peer"),
					Event::Commit,
				]
			}

			#[cfg(windows)]
			fn finalization_layer(
				name: &'static str,
				owns_job_cleanup: bool,
			) -> FinalizationLayerSpec {
				FinalizationLayerSpec {
					name,
					owns_job_cleanup,
				}
			}

			#[cfg(windows)]
			fn lifecycle_before_commit(name: &'static str) -> Vec<Event> {
				let mut events = successful_events(name);
				assert_eq!(events.pop(), Some(Event::Commit));
				events
			}

			#[cfg(windows)]
			fn successful_finalization_events(
				name: &'static str,
				layers: &[FinalizationLayerSpec],
			) -> Vec<Event> {
				let mut events = lifecycle_before_commit(name);
				events.extend(layers.iter().map(|layer| Event::FinalizeSpawn(layer.name)));
				events.extend(
					layers
						.iter()
						.map(|layer| Event::DisarmSpawnCleanup(layer.name)),
				);
				events.extend(
					layers
						.iter()
						.filter(|layer| !layer.owns_job_cleanup)
						.map(|layer| Event::DisarmNonOwner(layer.name)),
				);
				events.push(Event::Commit);
				if let Some(owner) = layers.iter().find(|layer| layer.owns_job_cleanup) {
					events.push(Event::DisarmOwner(owner.name));
				}
				events
			}

			fn expected_before(point: Point, name: &'static str) -> Vec<Event> {
				let mut events = vec![Event::Available(name)];
				if point == Point::Available {
					return events;
				}
				events.push(Event::ValidateCommand(name));
				if point == Point::ValidateCommand {
					return events;
				}
				events.push(Event::Pre(name));
				if point == Point::Pre {
					return events;
				}
				events.push(Event::Pre("peer"));
				if point == Point::ValidateAttempt {
					events.push(Event::ValidateAttempt(name));
					return events;
				}
				events.push(Event::ValidateAttempt(name));
				events.push(Event::Spawn(name));
				events
			}

			fn assert_failure(
				command: &mut CommandWrap,
				failure: Failure,
				expected_message: &'static str,
			) {
				match failure {
					Failure::Error(kind, _) => {
						let error = command.spawn().expect_err("the configured phase must fail");
						assert_eq!(error.kind(), kind);
						assert_eq!(error.to_string(), expected_message);
					}
					Failure::Panic(_) => {
						let panic = catch_unwind(AssertUnwindSafe(|| command.spawn()))
							.expect_err("the configured phase must panic");
						assert_eq!(
							*panic
								.downcast::<&'static str>()
								.expect("the test panic payload is a static string"),
							expected_message
						);
					}
				}
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

			#[test]
			#[cfg_attr(miri, ignore = "requires a native child process")]
			fn native_fallback_runs_the_complete_lifecycle() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = command();
				command.wrap(Peer {
					shared: Arc::clone(&shared),
					expect_provider: false,
					expect_custom_child: false,
				});

				wait_for_exit(command.spawn().unwrap());
				assert_eq!(
					shared.events(),
					vec![Event::Pre("peer"), Event::Post("peer"), Event::Wrap("peer")]
				);
			}

			#[test]
			fn provider_runs_the_exact_lifecycle_for_a_custom_child() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let child = command.spawn().unwrap();
				assert_eq!(child.as_ref().type_id(), TypeId::of::<CustomChild>());
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn provider_conflicts_precede_callbacks_and_allocation() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = provider_command(Arc::clone(&shared), "first");
				command.wrap(OtherProviderWrapper(Provider {
					name: "second",
					shared: Arc::clone(&shared),
				}));

				let error = command.spawn().expect_err("two providers must conflict");
				assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
				assert_eq!(error.to_string(), "multiple spawn providers are registered");
				assert!(shared.events().is_empty());
			}

			#[test]
			fn duplicate_provider_wrapper_extends_one_registration() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = command();
				configure_provider_intent(&mut command);
				command
					.wrap(ProviderWrapper::new("first", Arc::clone(&shared)))
					.wrap(ProviderWrapper::new("second", Arc::clone(&shared)))
					.wrap(Peer {
						shared: Arc::clone(&shared),
						expect_provider: true,
						expect_custom_child: true,
					});

				let _child = command.spawn().unwrap();
				let mut expected = vec![Event::Extend("first", "second")];
				expected.extend(successful_events("second"));
				assert_eq!(shared.events(), expected);
			}

			#[test]
			fn availability_error_precedes_native_only_rejection() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.fail_once(
					Point::Available,
					Failure::Error(io::ErrorKind::Unsupported, "provider unavailable"),
				);
				let mut command = provider_command(Arc::clone(&shared), "provider");
				let _ = command.native_mut();

				let error = command.spawn().expect_err("availability must fail first");
				assert_eq!(error.kind(), io::ErrorKind::Unsupported);
				assert_eq!(error.to_string(), "provider unavailable");
				assert_eq!(shared.events(), vec![Event::Available("provider")]);
			}

			#[test]
			fn provider_rejects_native_only_base_after_availability() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = provider_command(Arc::clone(&shared), "provider");
				let _ = command.native_mut();

				let error = command
					.spawn()
					.expect_err("native-only state must be rejected");
				assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
				assert_eq!(
					error.to_string(),
					"a spawn provider cannot use a native-only command"
				);
				assert_eq!(shared.events(), vec![Event::Available("provider")]);
			}

			#[test]
			fn immutable_validation_precedes_hooks_and_is_reusable() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.fail_once(
					Point::ValidateCommand,
					Failure::Error(io::ErrorKind::InvalidInput, "invalid command"),
				);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let error = command.spawn().expect_err("command validation must fail");
				assert_eq!(error.to_string(), "invalid command");
				assert_eq!(
					shared.events(),
					vec![
						Event::Available("provider"),
						Event::ValidateCommand("provider")
					]
				);

				shared.clear_events();
				let _child = command.spawn().unwrap();
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn attempt_validation_follows_hooks_and_is_reusable() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.fail_once(
					Point::ValidateAttempt,
					Failure::Error(io::ErrorKind::InvalidInput, "invalid attempt"),
				);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let error = command.spawn().expect_err("attempt validation must fail");
				assert_eq!(error.to_string(), "invalid attempt");
				assert_eq!(
					shared.events(),
					vec![
						Event::Available("provider"),
						Event::ValidateCommand("provider"),
						Event::Pre("provider"),
						Event::Pre("peer"),
						Event::ValidateAttempt("provider")
					]
				);

				shared.clear_events();
				let _child = command.spawn().unwrap();
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn provider_rejects_an_opaque_attempt_before_attempt_validation() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared
					.make_attempt_native_only
					.store(true, Ordering::SeqCst);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let error = command
					.spawn()
					.expect_err("opaque attempt must be rejected");
				assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
				assert_eq!(
					error.to_string(),
					"a spawn provider cannot use a native-only spawn attempt"
				);
				assert_eq!(
					shared.events(),
					vec![
						Event::Available("provider"),
						Event::ValidateCommand("provider"),
						Event::Pre("provider"),
						Event::Pre("peer")
					]
				);
			}

			#[test]
			fn explicit_spawners_cannot_bypass_a_provider() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let error = command
					.spawn_with(|_| panic!("explicit native spawner must not run"))
					.expect_err("spawn_with must reject a provider");
				assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
				assert_eq!(
					error.to_string(),
					"an explicit spawner cannot bypass a registered spawn provider"
				);
				let error = command
					.spawn_with_child(|_| panic!("explicit child spawner must not run"))
					.expect_err("spawn_with_child must reject a provider");
				assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
				assert!(shared.events().is_empty());

				let _child = command.spawn().unwrap();
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn provider_path_restores_after_errors_and_panics() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for point in [
					Point::Available,
					Point::ValidateCommand,
					Point::Pre,
					Point::ValidateAttempt,
					Point::Spawn,
				] {
					for failure in [
						Failure::Error(io::ErrorKind::Other, "provider callback failed"),
						Failure::Panic("provider callback failed"),
					] {
						let shared = Arc::new(Shared::default());
						shared.fail_once(point, failure);
						let mut command = provider_command(Arc::clone(&shared), "provider");

						assert_failure(&mut command, failure, "provider callback failed");
						assert_eq!(shared.events(), expected_before(point, "provider"));
						assert!(command.get_wrap::<ProviderWrapper>().is_some());
						assert!(command.get_wrap::<Peer>().is_some());

						shared.clear_events();
						let _child = command.spawn().unwrap();
						assert_eq!(shared.events(), successful_events("provider"));
					}
				}
			}

			#[test]
			fn hook_failures_roll_back_and_restore_for_reuse() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for point in [Point::Post, Point::PeerPost, Point::Wrap, Point::PeerWrap] {
					for failure in [
						Failure::Error(io::ErrorKind::Other, "hook failed"),
						Failure::Panic("hook failed"),
					] {
						let shared = Arc::new(Shared::default());
						shared.fail_once(point, failure);
						let mut command = provider_command(Arc::clone(&shared), "provider");

						assert_failure(&mut command, failure, "hook failed");
						let mut expected = expected_before(Point::Spawn, "provider");
						expected.push(Event::Post("provider"));
						if point != Point::Post {
							expected.push(Event::Post("peer"));
						}
						if matches!(point, Point::Wrap | Point::PeerWrap) {
							expected.push(Event::Wrap("provider"));
						}
						if point == Point::PeerWrap {
							expected.push(Event::Wrap("peer"));
						}
						expected.push(Event::Rollback);
						assert_eq!(shared.events(), expected);

						shared.clear_events();
						let _child = command.spawn().unwrap();
						assert_eq!(shared.events(), successful_events("provider"));
					}
				}
			}

			#[test]
			fn rollback_failures_preserve_the_original_failure() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for original in [
					Failure::Error(io::ErrorKind::Other, "original failure"),
					Failure::Panic("original failure"),
				] {
					for cleanup in [
						Failure::Error(io::ErrorKind::Other, "rollback failure"),
						Failure::Panic("rollback failure"),
					] {
						let shared = Arc::new(Shared::default());
						shared.fail_once(Point::Post, original);
						shared.fail_once(Point::Rollback, cleanup);
						let mut command = provider_command(Arc::clone(&shared), "provider");

						assert_failure(&mut command, original, "original failure");
						assert_eq!(shared.events().last(), Some(&Event::Rollback));

						shared.clear_events();
						let _child = command.spawn().unwrap();
						assert_eq!(shared.events(), successful_events("provider"));
					}
				}
			}

			#[test]
			fn commit_failure_rolls_back_and_preserves_its_error() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.fail_once(
					Point::Commit,
					Failure::Error(io::ErrorKind::Other, "commit failed"),
				);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let error = command.spawn().expect_err("commit must fail");
				assert_eq!(error.to_string(), "commit failed");
				let mut expected = successful_events("provider");
				expected.push(Event::Rollback);
				assert_eq!(shared.events(), expected);

				shared.clear_events();
				let _child = command.spawn().unwrap();
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[cfg(windows)]
			#[test]
			fn precommit_child_hook_failures_roll_back_preserve_failure_and_allow_reuse() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for (point, failure_event) in [
					(Point::FinalizeSpawn, Event::FinalizeSpawn("layer")),
					(
						Point::DisarmSpawnCleanup,
						Event::DisarmSpawnCleanup("layer"),
					),
					(Point::DisarmNonOwner, Event::DisarmNonOwner("layer")),
				] {
					for failure in [
						Failure::Error(io::ErrorKind::Other, "finalization failed"),
						Failure::Panic("finalization failed"),
					] {
						let shared = Arc::new(Shared::default());
						let layers = vec![finalization_layer("layer", false)];
						shared.set_finalization_layers(layers.clone());
						shared.fail_once(point, failure);
						let mut command = provider_command(Arc::clone(&shared), "provider");

						assert_failure(&mut command, failure, "finalization failed");
						let events = shared.events();
						assert_eq!(events.last(), Some(&Event::Rollback));
						assert!(events.contains(&failure_event));
						assert!(!events.contains(&Event::Commit));
						assert!(
							events
								.windows(2)
								.any(|events| events == [failure_event, Event::Rollback]),
							"the failing hook must be followed by provider rollback: {events:?}"
						);

						shared.clear_events();
						let child = command.spawn().unwrap();
						drop(child);
						assert_eq!(
							shared.events(),
							successful_finalization_events("provider", &layers)
						);
					}
				}
			}

			#[cfg(windows)]
			#[test]
			fn provider_commit_separates_nonowners_from_the_sole_final_owner() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for layers in [
					vec![
						finalization_layer("outer", false),
						finalization_layer("inner", false),
					],
					vec![
						finalization_layer("outer", false),
						finalization_layer("owner", true),
						finalization_layer("inner", false),
					],
				] {
					let shared = Arc::new(Shared::default());
					shared.set_finalization_layers(layers.clone());
					let mut command = provider_command(Arc::clone(&shared), "provider");

					let child = command.spawn().unwrap();
					drop(child);
					let mut expected = successful_finalization_events("provider", &layers);
					if let Some(owner) = layers.iter().find(|layer| layer.owns_job_cleanup) {
						expected.push(Event::OwnerDropped(owner.name, false));
					}
					assert_eq!(shared.events(), expected);
				}
			}

			#[cfg(windows)]
			#[test]
			fn multiple_final_owners_fail_and_roll_back_before_commit() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let layers = vec![
					finalization_layer("outer-owner", true),
					finalization_layer("nonowner", false),
					finalization_layer("inner-owner", true),
				];
				shared.set_finalization_layers(layers);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let error = command.spawn().expect_err("multiple owners must fail");
				assert_eq!(
					error.to_string(),
					"multiple child layers own JobObject cleanup"
				);
				let events = shared.events();
				assert!(!events.contains(&Event::Commit));
				assert_eq!(events.last(), Some(&Event::Rollback));
				assert!(events.contains(&Event::DisarmNonOwner("nonowner")));
				assert!(events.contains(&Event::OwnerDropped("outer-owner", true)));
				assert!(events.contains(&Event::OwnerDropped("inner-owner", true)));
			}

			#[cfg(windows)]
			#[test]
			fn final_owner_failures_leave_the_owner_armed_and_preserve_the_failure() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for failure in [
					Failure::Error(io::ErrorKind::Other, "owner disarm failed"),
					Failure::Panic("owner disarm failed"),
				] {
					let shared = Arc::new(Shared::default());
					let layers = vec![finalization_layer("owner", true)];
					shared.set_finalization_layers(layers.clone());
					shared.fail_once(Point::DisarmOwner, failure);
					let mut command = provider_command(Arc::clone(&shared), "provider");

					assert_failure(&mut command, failure, "owner disarm failed");
					let events = shared.events();
					assert!(events.contains(&Event::Commit));
					assert!(events.contains(&Event::DisarmOwner("owner")));
					assert_eq!(events.last(), Some(&Event::OwnerDropped("owner", true)));
					assert!(!events.contains(&Event::Rollback));

					shared.clear_events();
					let child = command.spawn().unwrap();
					drop(child);
					let mut expected = successful_finalization_events("provider", &layers);
					expected.push(Event::OwnerDropped("owner", false));
					assert_eq!(shared.events(), expected);
				}
			}

			#[test]
			fn commit_panic_rolls_back_preserves_payload_and_allows_reuse() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.fail_once(Point::Commit, Failure::Panic("commit failed"));
				let mut command = provider_command(Arc::clone(&shared), "provider");

				assert_failure(
					&mut command,
					Failure::Panic("commit failed"),
					"commit failed",
				);
				let mut expected = successful_events("provider");
				expected.push(Event::Rollback);
				assert_eq!(shared.events(), expected);

				shared.clear_events();
				let _child = command.spawn().unwrap();
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn successful_provider_can_be_reused() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let mut command = provider_command(Arc::clone(&shared), "provider");

				for _ in 0..2 {
					let _child = command.spawn().unwrap();
				}
				let expected = successful_events("provider")
					.into_iter()
					.chain(successful_events("provider"))
					.collect::<Vec<_>>();
				assert_eq!(shared.events(), expected);
			}
		}
	};
}

#[cfg(all(feature = "tokio1", feature = "kill-on-drop"))]
mod tokio_kill_on_drop_policy {
	use std::io;

	use process_wrap::tokio::{
		Command, CommandWrapper, KillOnDrop, ProviderProduct, SpawnAttempt, SpawnProvider,
	};

	#[derive(Debug)]
	struct InspectProvider;

	impl SpawnProvider for InspectProvider {
		fn validate_attempt(&self, attempt: &SpawnAttempt, _command: &Command) -> io::Result<()> {
			assert!(!attempt.is_native_only());
			assert!(attempt.kills_on_drop());
			Err(io::Error::other("kill-on-drop policy inspected"))
		}

		fn spawn(
			&self,
			_attempt: &mut SpawnAttempt,
			_command: &Command,
		) -> io::Result<ProviderProduct> {
			unreachable!("attempt validation stops before provider allocation")
		}
	}

	#[derive(Debug)]
	struct Provider(InspectProvider);

	impl CommandWrapper for Provider {
		fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
			Some(&self.0)
		}
	}

	#[test]
	fn kill_on_drop_remains_portable_for_tokio_providers() {
		let mut command = Command::new("provider-owned-program");
		command.wrap(KillOnDrop).wrap(Provider(InspectProvider));

		let error = command
			.spawn()
			.expect_err("policy inspection must stop before provider allocation");
		assert_eq!(error.to_string(), "kill-on-drop policy inspected");
	}
}

#[cfg(all(unix, feature = "std"))]
const _: Option<process_wrap::std::ProcessGroupTarget> = None;
#[cfg(all(unix, feature = "tokio1"))]
const _: Option<process_wrap::tokio::ProcessGroupTarget> = None;
#[cfg(all(windows, feature = "std"))]
const _: Option<process_wrap::std::WindowsSpawnPolicy> = None;
#[cfg(all(windows, feature = "tokio1"))]
const _: Option<process_wrap::tokio::WindowsSpawnPolicy> = None;

#[cfg(feature = "std")]
spawn_provider_tests!(
	std_frontend,
	process_wrap::std::CommandWrap,
	process_wrap::std::SpawnAttempt,
	process_wrap::std::CommandWrapper,
	process_wrap::std::ChildWrapper,
	process_wrap::std::ProviderProduct,
	process_wrap::std::SpawnProvider,
	None
);

#[cfg(feature = "tokio1")]
spawn_provider_tests!(
	tokio_frontend,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	process_wrap::tokio::ProviderProduct,
	process_wrap::tokio::SpawnProvider,
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);
