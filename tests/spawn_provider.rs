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
					atomic::{AtomicBool, AtomicUsize, Ordering},
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

			#[derive(Debug)]
			struct CommittedDropPayload(Arc<()>);

			#[derive(Debug)]
			struct PanickingDropPayload {
				drops: Arc<AtomicUsize>,
			}

			impl Drop for PanickingDropPayload {
				fn drop(&mut self) {
					self.drops.fetch_add(1, Ordering::SeqCst);
					panic_any("a secondary panic payload was dropped");
				}
			}

			#[derive(Debug)]
			enum RollbackBehavior {
				Error,
				Panic(PanickingDropPayload),
			}

			#[derive(Debug)]
			enum TransactionDropBehavior {
				Record,
				PanicCommitted(CommittedDropPayload),
				PanicSecondary(PanickingDropPayload),
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
				TransactionDrop,
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
				rollback_behavior: Mutex<Option<RollbackBehavior>>,
				transaction_drop_behavior: Mutex<Option<TransactionDropBehavior>>,
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

				fn set_rollback_behavior(&self, behavior: RollbackBehavior) {
					*self.rollback_behavior.lock().unwrap() = Some(behavior);
				}

				fn take_rollback_behavior(&self) -> Option<RollbackBehavior> {
					self.rollback_behavior.lock().unwrap().take()
				}

				fn set_transaction_drop_behavior(&self, behavior: TransactionDropBehavior) {
					*self.transaction_drop_behavior.lock().unwrap() = Some(behavior);
				}

				fn take_transaction_drop_behavior(&self) -> Option<TransactionDropBehavior> {
					self.transaction_drop_behavior.lock().unwrap().take()
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

			fn successful_exit_status() -> std::process::ExitStatus {
				#[cfg(unix)]
				{
					use std::os::unix::process::ExitStatusExt;
					std::process::ExitStatus::from_raw(0)
				}

				#[cfg(windows)]
				{
					use std::os::windows::process::ExitStatusExt;
					std::process::ExitStatus::from_raw(0)
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

				fn try_clone(&self) -> Option<Box<dyn ChildWrapper>> {
					Some(Box::new(Self))
				}

				fn start_kill(&mut self) -> io::Result<()> {
					Ok(())
				}

				fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
					Ok(Some(successful_exit_status()))
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
			struct Transaction {
				shared: Arc<Shared>,
				committed: bool,
			}

			impl Transaction {
				fn new(shared: Arc<Shared>) -> Self {
					Self {
						shared,
						committed: false,
					}
				}
			}

			impl SpawnTransaction for Transaction {
				fn commit(&mut self) -> io::Result<()> {
					self.shared.event(Event::Commit);
					self.shared.fail(Point::Commit)?;
					self.committed = true;
					Ok(())
				}

				fn rollback(&mut self) -> io::Result<()> {
					self.shared.event(Event::Rollback);
					self.shared.fail(Point::Rollback)?;
					match self.shared.take_rollback_behavior() {
						Some(RollbackBehavior::Error) => Err(io::Error::other("rollback failed")),
						Some(RollbackBehavior::Panic(payload)) => panic_any(payload),
						None => Ok(()),
					}
				}
			}

			impl Drop for Transaction {
				fn drop(&mut self) {
					let Some(behavior) = self.shared.take_transaction_drop_behavior() else {
						return;
					};
					self.shared.event(Event::TransactionDrop);
					match behavior {
						TransactionDropBehavior::Record => {}
						TransactionDropBehavior::PanicCommitted(payload) => {
							assert!(self.committed, "the transaction must commit before deferred disposal");
							panic_any(payload);
						}
						TransactionDropBehavior::PanicSecondary(payload) => panic_any(payload),
					}
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
						Box::new(Transaction::new(Arc::clone(&self.shared))),
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
			struct IdentityError(Arc<()>);

			impl std::fmt::Display for IdentityError {
				fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
					formatter.write_str("primary error")
				}
			}

			impl std::error::Error for IdentityError {}

			#[derive(Debug)]
			struct PrimaryPanic(Arc<()>);

			#[derive(Debug)]
			enum PrimaryFailure {
				Error(Arc<()>),
				Panic(Arc<()>),
			}

			#[derive(Debug)]
			struct PrimaryFailureWrapper(Mutex<Option<PrimaryFailure>>);

			impl PrimaryFailureWrapper {
				fn new(failure: PrimaryFailure) -> Self {
					Self(Mutex::new(Some(failure)))
				}
			}

			impl CommandWrapper for PrimaryFailureWrapper {
				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_command: &CommandWrap,
				) -> io::Result<()> {
					let failure = self.0.lock().unwrap().take();
					match failure {
						Some(PrimaryFailure::Error(identity)) => {
							Err(io::Error::other(IdentityError(identity)))
						}
						Some(PrimaryFailure::Panic(identity)) => panic_any(PrimaryPanic(identity)),
						None => Ok(()),
					}
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

			fn provider_command_with_primary_failure(
				shared: Arc<Shared>,
				failure: PrimaryFailure,
			) -> CommandWrap {
				let mut command = provider_command(shared, "provider");
				command.wrap(PrimaryFailureWrapper::new(failure));
				command
			}

			fn assert_primary_failure_preserved(
				outcome: std::thread::Result<io::Result<Box<dyn ChildWrapper>>>,
				identity: &Arc<()>,
				was_panic: bool,
			) {
				if was_panic {
					let payload = outcome.expect_err("the primary panic must be resumed");
					let payload = payload
						.downcast::<PrimaryPanic>()
						.expect("cleanup must not replace the primary panic payload");
					assert!(Arc::ptr_eq(&payload.0, identity));
				} else {
					let error = outcome
						.expect("cleanup must not replace the primary error with a panic")
						.expect_err("the primary error must be returned");
					let payload = error
						.get_ref()
						.and_then(|error| error.downcast_ref::<IdentityError>())
						.expect("cleanup must preserve the primary io::Error payload");
					assert!(Arc::ptr_eq(&payload.0, identity));
				}
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
				assert_eq!(child.inner().type_id(), TypeId::of::<CustomChild>());
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn committed_transaction_residue_moves_with_the_child_and_supported_clones() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let identity = Arc::new(());
				shared.set_transaction_drop_behavior(TransactionDropBehavior::PanicCommitted(
					CommittedDropPayload(Arc::clone(&identity)),
				));
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				let mut child = match outcome {
					Ok(Ok(child)) => child,
					Ok(Err(error)) => panic!("committed provider spawn failed: {error}"),
					Err(payload) => {
						let payload = payload
							.downcast::<CommittedDropPayload>()
							.expect("the committed transaction supplied the panic payload");
						assert!(Arc::ptr_eq(&payload.0, &identity));
						assert!(!shared.events().contains(&Event::Rollback));
						panic!("committed transaction residue was destroyed before child transfer");
					}
				};

				assert_eq!(shared.events(), successful_events("provider"));
				assert_eq!(child.inner().type_id(), TypeId::of::<CustomChild>());
				child.start_kill().expect("terminate returned child");
				let first = child
					.try_wait()
					.expect("reap returned child")
					.expect("returned child is complete");
				assert_eq!(child.try_wait().expect("repeat child status"), Some(first));

				let clone = child
					.try_clone()
					.expect("the child layer supports cloning");
				drop(child);
				assert!(!shared.events().contains(&Event::TransactionDrop));
				let payload = catch_unwind(AssertUnwindSafe(|| drop(clone)))
					.expect_err("the last child owner destroys the transaction residue");
				let payload = payload
					.downcast::<CommittedDropPayload>()
					.expect("the exact deferred transaction payload is preserved");
				assert!(Arc::ptr_eq(&payload.0, &identity));
				assert_eq!(shared.events().last(), Some(&Event::TransactionDrop));
				assert!(!shared.events().contains(&Event::Rollback));

				shared.clear_events();
				let mut child = command.spawn().expect("the command remains reusable");
				child.start_kill().expect("terminate reused child");
				assert!(child.try_wait().expect("reap reused child").is_some());
				drop(child);
				assert_eq!(shared.events(), successful_events("provider"));
			}

			#[test]
			fn consuming_the_provider_sidecar_disposes_ordinary_residue() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.set_transaction_drop_behavior(TransactionDropBehavior::Record);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let child = command.spawn().expect("spawn provider child");
				assert!(!shared.events().contains(&Event::TransactionDrop));
				let child = child.into_inner();
				assert_eq!(child.as_ref().type_id(), TypeId::of::<CustomChild>());
				assert_eq!(shared.events().last(), Some(&Event::TransactionDrop));
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
			fn rollback_errors_preserve_exact_primary_errors_and_panics() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for was_panic in [false, true] {
					let shared = Arc::new(Shared::default());
					shared.set_rollback_behavior(RollbackBehavior::Error);
					let identity = Arc::new(());
					let failure = if was_panic {
						PrimaryFailure::Panic(Arc::clone(&identity))
					} else {
						PrimaryFailure::Error(Arc::clone(&identity))
					};
					let mut command =
						provider_command_with_primary_failure(Arc::clone(&shared), failure);

					let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
					assert_primary_failure_preserved(outcome, &identity, was_panic);
					assert_eq!(shared.events().last(), Some(&Event::Rollback));

					shared.clear_events();
					drop(command.spawn().expect("the command remains reusable"));
					assert_eq!(shared.events(), successful_events("provider"));
				}
			}

			#[test]
			fn rollback_panic_payloads_are_quarantined_without_replacing_the_primary() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for was_panic in [false, true] {
					let shared = Arc::new(Shared::default());
					let secondary_drops = Arc::new(AtomicUsize::new(0));
					shared.set_rollback_behavior(RollbackBehavior::Panic(PanickingDropPayload {
						drops: Arc::clone(&secondary_drops),
					}));
					let identity = Arc::new(());
					let failure = if was_panic {
						PrimaryFailure::Panic(Arc::clone(&identity))
					} else {
						PrimaryFailure::Error(Arc::clone(&identity))
					};
					let mut command =
						provider_command_with_primary_failure(Arc::clone(&shared), failure);

					let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
					assert_primary_failure_preserved(outcome, &identity, was_panic);
					assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
					assert_eq!(shared.events().last(), Some(&Event::Rollback));

					shared.clear_events();
					drop(command.spawn().expect("the command remains reusable"));
					assert_eq!(shared.events(), successful_events("provider"));
				}
			}

			#[test]
			fn transaction_drop_panic_payloads_are_quarantined_after_rollback() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for was_panic in [false, true] {
					let shared = Arc::new(Shared::default());
					let secondary_drops = Arc::new(AtomicUsize::new(0));
					shared.set_transaction_drop_behavior(
						TransactionDropBehavior::PanicSecondary(PanickingDropPayload {
							drops: Arc::clone(&secondary_drops),
						}),
					);
					let identity = Arc::new(());
					let failure = if was_panic {
						PrimaryFailure::Panic(Arc::clone(&identity))
					} else {
						PrimaryFailure::Error(Arc::clone(&identity))
					};
					let mut command =
						provider_command_with_primary_failure(Arc::clone(&shared), failure);

					let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
					assert_primary_failure_preserved(outcome, &identity, was_panic);
					assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
					assert!(shared.events().ends_with(&[Event::Rollback, Event::TransactionDrop]));

					shared.clear_events();
					drop(command.spawn().expect("the command remains reusable"));
					assert_eq!(shared.events(), successful_events("provider"));
				}
			}

			#[test]
			fn rollback_and_transaction_unwinds_are_separated() {
				const CHILD_ENV: &str = "PROCESS_WRAP_SEPARATE_PROVIDER_UNWINDS";
				let child_value = stringify!($module);
				if std::env::var_os(CHILD_ENV).as_deref() != Some(OsStr::new(child_value)) {
					let output = std::process::Command::new(std::env::current_exe().unwrap())
						.args([
							"--exact",
							concat!(stringify!($module), "::rollback_and_transaction_unwinds_are_separated"),
							"--nocapture",
						])
						.env(CHILD_ENV, child_value)
						.output()
						.expect("start isolated lifecycle regression");
					assert!(
						output.status.success(),
						"rollback and transaction disposal overlapped:\nstdout:\n{}\nstderr:\n{}",
						String::from_utf8_lossy(&output.stdout),
						String::from_utf8_lossy(&output.stderr),
					);
					return;
				}

				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let rollback_payload_drops = Arc::new(AtomicUsize::new(0));
				let transaction_payload_drops = Arc::new(AtomicUsize::new(0));
				shared.set_rollback_behavior(RollbackBehavior::Panic(PanickingDropPayload {
					drops: Arc::clone(&rollback_payload_drops),
				}));
				shared.set_transaction_drop_behavior(TransactionDropBehavior::PanicSecondary(
					PanickingDropPayload {
						drops: Arc::clone(&transaction_payload_drops),
					},
				));
				let identity = Arc::new(());
				let mut command = provider_command_with_primary_failure(
					Arc::clone(&shared),
					PrimaryFailure::Panic(Arc::clone(&identity)),
				);

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				assert_primary_failure_preserved(outcome, &identity, true);
				assert_eq!(rollback_payload_drops.load(Ordering::SeqCst), 0);
				assert_eq!(transaction_payload_drops.load(Ordering::SeqCst), 0);
				assert!(shared.events().ends_with(&[Event::Rollback, Event::TransactionDrop]));
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

			#[cfg(windows)]
			#[test]
			fn final_owner_failures_preserve_the_owner_failure_over_residue_disposal() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for failure in [
					Failure::Error(io::ErrorKind::Other, "owner disarm failed"),
					Failure::Panic("owner disarm failed"),
				] {
					let shared = Arc::new(Shared::default());
					let secondary_drops = Arc::new(AtomicUsize::new(0));
					let layers = vec![finalization_layer("owner", true)];
					shared.set_finalization_layers(layers.clone());
					shared.fail_once(Point::DisarmOwner, failure);
					shared.set_transaction_drop_behavior(
						TransactionDropBehavior::PanicSecondary(PanickingDropPayload {
							drops: Arc::clone(&secondary_drops),
						}),
					);
					let mut command = provider_command(Arc::clone(&shared), "provider");

					assert_failure(&mut command, failure, "owner disarm failed");
					let events = shared.events();
					assert!(events.contains(&Event::Commit));
					assert!(events.contains(&Event::DisarmOwner("owner")));
					assert!(events.ends_with(&[
						Event::OwnerDropped("owner", true),
						Event::TransactionDrop,
					]));
					assert!(!events.contains(&Event::Rollback));
					assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);

					shared.clear_events();
					let child = command.spawn().expect("the command remains reusable");
					drop(child);
					let mut expected = successful_finalization_events("provider", &layers);
					expected.push(Event::OwnerDropped("owner", false));
					assert_eq!(shared.events(), expected);
				}
			}

			#[cfg(windows)]
			#[test]
			fn final_owner_disarms_before_committed_residue_transfer() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let identity = Arc::new(());
				let layers = vec![finalization_layer("owner", true)];
				shared.set_finalization_layers(layers.clone());
				shared.set_transaction_drop_behavior(TransactionDropBehavior::PanicCommitted(
					CommittedDropPayload(Arc::clone(&identity)),
				));
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let child = command.spawn().expect("spawn provider child");
				assert_eq!(
					shared.events(),
					successful_finalization_events("provider", &layers)
				);
				let payload = catch_unwind(AssertUnwindSafe(|| drop(child)))
					.expect_err("child disposal destroys the committed residue");
				let payload = payload
					.downcast::<CommittedDropPayload>()
					.expect("the exact deferred transaction payload is preserved");
				assert!(Arc::ptr_eq(&payload.0, &identity));
				assert_eq!(
					shared.events().as_slice(),
					&[
						successful_finalization_events("provider", &layers).as_slice(),
						&[Event::OwnerDropped("owner", false), Event::TransactionDrop],
					]
					.concat()
				);
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

macro_rules! std_shared_process_methods {
	() => {
		fn id(&self) -> u32 {
			self.child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.id()
		}

		fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
			self.child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.wait()
		}
	};
}

macro_rules! tokio_shared_process_methods {
	() => {
		fn id(&self) -> Option<u32> {
			self.child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.id()
		}

		fn wait(
			&mut self,
		) -> std::pin::Pin<
			Box<
				dyn std::future::Future<Output = std::io::Result<std::process::ExitStatus>>
					+ Send
					+ '_,
			>,
		> {
			let child = std::sync::Arc::clone(&self.child);
			Box::pin(std::future::poll_fn(move |context| {
				let mut child = child
					.lock()
					.unwrap_or_else(std::sync::PoisonError::into_inner);
				let mut wait = child.wait();
				std::future::Future::poll(wait.as_mut(), context)
			}))
		}
	};
}

macro_rules! std_wait_for_child {
	($runtime:expr, $child:expr) => {{
		let _ = &$runtime;
		$child.wait()
	}};
}

macro_rules! tokio_wait_for_child {
	($runtime:expr, $child:expr) => {
		$runtime
			.as_ref()
			.expect("the Tokio frontend has a runtime")
			.block_on($child.wait())
	};
}

macro_rules! provider_capability_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$provider_product:path,
		$spawn_provider:path,
		$native_command:path,
		$native_child:path,
		$wait_for_child:ident,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				any::TypeId,
				io,
				process::Stdio,
				sync::{
					Arc,
					atomic::{AtomicUsize, Ordering},
				},
				thread::sleep,
				time::{Duration, Instant},
			};

			use process_wrap::SpawnTransaction;
			use $child_wrapper as ChildWrapper;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $provider_product as ProviderProduct;
			use $spawn_attempt as SpawnAttempt;
			use $spawn_provider as SpawnProvider;

			#[derive(Debug)]
			struct CompletedTransaction {
				drops: Arc<AtomicUsize>,
				committed: bool,
			}

			impl SpawnTransaction for CompletedTransaction {
				fn commit(&mut self) -> io::Result<()> {
					self.committed = true;
					Ok(())
				}

				fn rollback(&mut self) -> io::Result<()> {
					Ok(())
				}
			}

			impl Drop for CompletedTransaction {
				fn drop(&mut self) {
					assert!(
						self.committed,
						"the completed-child transaction must commit"
					);
					self.drops.fetch_add(1, Ordering::SeqCst);
				}
			}

			#[derive(Debug)]
			struct Provider {
				drops: Arc<AtomicUsize>,
			}

			impl SpawnProvider for Provider {
				fn spawn(
					&self,
					_attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<ProviderProduct> {
					let mut command = <$native_command>::new(std::env::current_exe()?);
					command
						.args([
							"--exact",
							concat!(stringify!($module), "::completed_child_process"),
							"--nocapture",
						])
						.stdin(Stdio::piped())
						.stdout(Stdio::piped())
						.stderr(Stdio::piped());
					let mut child = command.spawn()?;
					let deadline = Instant::now() + Duration::from_secs(5);
					while child.try_wait()?.is_none() {
						if Instant::now() >= deadline {
							return Err(io::Error::new(
								io::ErrorKind::TimedOut,
								"capability child did not exit",
							));
						}
						sleep(Duration::from_millis(5));
					}
					Ok(ProviderProduct::new(
						Box::new(child),
						Box::new(CompletedTransaction {
							drops: Arc::clone(&self.drops),
							committed: false,
						}),
					))
				}
			}

			#[derive(Debug)]
			struct ProviderWrapper(Provider);

			impl CommandWrapper for ProviderWrapper {
				fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
					Some(&self.0)
				}
			}

			#[derive(Debug)]
			struct OuterLayer(Box<dyn ChildWrapper>);

			impl ChildWrapper for OuterLayer {
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
			struct OuterWrapper;

			impl CommandWrapper for OuterWrapper {
				fn wrap_child(
					&mut self,
					child: Box<dyn ChildWrapper>,
					_command: &CommandWrap,
				) -> io::Result<Box<dyn ChildWrapper>> {
					Ok(Box::new(OuterLayer(child)))
				}
			}

			fn runtime() -> Option<tokio::runtime::Runtime> {
				$runtime
			}

			fn command(drops: Arc<AtomicUsize>) -> CommandWrap {
				let mut command = CommandWrap::new("provider-owned-program");
				command
					.wrap(ProviderWrapper(Provider { drops }))
					.wrap(OuterWrapper);
				command
			}

			#[test]
			fn completed_child_process() {}

			#[test]
			#[cfg_attr(miri, ignore = "requires native child processes")]
			fn committed_sidecar_delegates_the_complete_native_child_contract() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let drops = Arc::new(AtomicUsize::new(0));
				let mut command = command(Arc::clone(&drops));

				let mut child = command.spawn().expect("spawn completed provider child");
				assert_eq!(drops.load(Ordering::SeqCst), 0);
				assert_eq!(child.inner().type_id(), TypeId::of::<OuterLayer>());
				assert_eq!(
					child.inner().inner().type_id(),
					TypeId::of::<$native_child>()
				);
				assert!(child.stdin().is_some());
				assert!(child.stdout().is_some());
				assert!(child.stderr().is_some());
				let native_id = child
					.try_inner_child()
					.expect("traverse to immutable native child")
					.id();
				assert_eq!(child.id(), native_id);
				// SAFETY: the process is complete and the forwarding layer owns no supervision state.
				assert!(unsafe { child.try_inner_child_mut() }.is_some());
				let first = $wait_for_child!(runtime, child).expect("first repeated wait");
				let second = $wait_for_child!(runtime, child).expect("second repeated wait");
				assert_eq!(first, second);
				assert_eq!(child.try_wait().expect("repeat try_wait"), Some(first));
				assert!(child.try_clone().is_none());
				#[cfg(windows)]
				assert!(child.try_process_handle().is_some());
				drop(child);
				assert_eq!(drops.load(Ordering::SeqCst), 1);

				let child = command.spawn().expect("spawn child for native extraction");
				// SAFETY: the process is complete, the transaction has no cleanup resources after commit,
				// and the forwarding layer owns no supervision state.
				let mut child =
					unsafe { child.try_into_inner_child() }.expect("extract native child");
				assert_eq!(drops.load(Ordering::SeqCst), 2);
				let first = $wait_for_child!(runtime, child).expect("wait extracted native child");
				let second = $wait_for_child!(runtime, child).expect("repeat extracted child wait");
				assert_eq!(first, second);
			}
		}
	};
}

macro_rules! real_provider_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$provider_product:path,
		$spawn_provider:path,
		$native_command:path,
		$shared_process_methods:ident,
		$wait_for_child:ident,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				fs, io,
				panic::{AssertUnwindSafe, catch_unwind, panic_any},
				path::{Path, PathBuf},
				sync::{
					Arc, Mutex,
					atomic::{AtomicUsize, Ordering},
				},
				thread::sleep,
				time::{Duration, Instant},
			};

			use process_wrap::SpawnTransaction;
			use $child_wrapper as ChildWrapper;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $provider_product as ProviderProduct;
			use $spawn_attempt as SpawnAttempt;
			use $spawn_provider as SpawnProvider;

			const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
			const MARKER_ENV: &str = "PROCESS_WRAP_PROVIDER_MARKER_DIR";

			#[derive(Clone, Debug)]
			struct MarkerPaths {
				directory: PathBuf,
				ready: PathBuf,
				go: PathBuf,
				marker: PathBuf,
			}

			impl MarkerPaths {
				fn new(directory: &Path) -> Self {
					Self {
						directory: directory.to_owned(),
						ready: directory.join("ready"),
						go: directory.join("go"),
						marker: directory.join("marker"),
					}
				}
			}

			#[derive(Clone, Copy, Debug, Eq, PartialEq)]
			enum Event {
				Commit,
				Rollback,
				TransactionDrop,
			}

			#[derive(Debug)]
			struct DeferredPayload(Arc<()>);

			#[derive(Debug)]
			struct PanickingDropPayload(Arc<AtomicUsize>);

			impl Drop for PanickingDropPayload {
				fn drop(&mut self) {
					self.0.fetch_add(1, Ordering::SeqCst);
					panic_any("a secondary process-transaction payload was dropped");
				}
			}

			#[derive(Debug)]
			enum DropBehavior {
				PanicCommitted(DeferredPayload),
				PanicSecondary(PanickingDropPayload),
			}

			#[derive(Debug, Default)]
			struct Shared {
				events: Mutex<Vec<Event>>,
				paths: Mutex<Option<MarkerPaths>>,
				last_child: Mutex<Option<Arc<Mutex<Box<dyn ChildWrapper>>>>>,
				drop_behavior: Mutex<Option<DropBehavior>>,
			}

			impl Shared {
				fn event(&self, event: Event) {
					self.events.lock().unwrap().push(event);
				}

				fn events(&self) -> Vec<Event> {
					self.events.lock().unwrap().clone()
				}

				fn set_paths(&self, paths: MarkerPaths) {
					*self.paths.lock().unwrap() = Some(paths);
				}

				fn paths(&self) -> MarkerPaths {
					self.paths
						.lock()
						.unwrap()
						.clone()
						.expect("the process fixture has marker paths")
				}

				fn set_drop_behavior(&self, behavior: DropBehavior) {
					*self.drop_behavior.lock().unwrap() = Some(behavior);
				}

				fn take_drop_behavior(&self) -> Option<DropBehavior> {
					self.drop_behavior.lock().unwrap().take()
				}

				fn child(&self) -> Arc<Mutex<Box<dyn ChildWrapper>>> {
					Arc::clone(
						self.last_child
							.lock()
							.unwrap()
							.as_ref()
							.expect("the provider published its process child"),
					)
				}

				fn clear_child(&self) {
					self.last_child.lock().unwrap().take();
				}
			}

			#[derive(Debug)]
			struct SharedProcessChild {
				child: Arc<Mutex<Box<dyn ChildWrapper>>>,
			}

			impl ChildWrapper for SharedProcessChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self
				}

				fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
					self
				}

				fn start_kill(&mut self) -> io::Result<()> {
					self.child
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.start_kill()
				}

				fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
					self.child
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.try_wait()
				}

				$shared_process_methods!();
			}

			#[derive(Debug)]
			struct Transaction {
				cleanup: Option<Arc<Mutex<Box<dyn ChildWrapper>>>>,
				shared: Arc<Shared>,
				committed: bool,
				drop_behavior: Option<DropBehavior>,
			}

			impl Transaction {
				fn terminate_and_reap(&mut self) -> io::Result<()> {
					let Some(child) = self.cleanup.take() else {
						return Ok(());
					};
					let mut child = child
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner);
					match child.start_kill() {
						Ok(()) => {}
						Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
						Err(error) => return Err(error),
					}
					let deadline = Instant::now() + EXIT_TIMEOUT;
					loop {
						if child.try_wait()?.is_some() {
							return Ok(());
						}
						if Instant::now() >= deadline {
							return Err(io::Error::new(
								io::ErrorKind::TimedOut,
								"provider rollback did not reap its child",
							));
						}
						sleep(Duration::from_millis(5));
					}
				}
			}

			impl SpawnTransaction for Transaction {
				fn commit(&mut self) -> io::Result<()> {
					self.shared.event(Event::Commit);
					self.cleanup.take();
					self.committed = true;
					Ok(())
				}

				fn rollback(&mut self) -> io::Result<()> {
					self.shared.event(Event::Rollback);
					self.terminate_and_reap()
				}
			}

			impl Drop for Transaction {
				fn drop(&mut self) {
					let Some(behavior) = self.drop_behavior.take() else {
						return;
					};
					self.shared.event(Event::TransactionDrop);
					match behavior {
						DropBehavior::PanicCommitted(payload) => {
							assert!(self.committed, "the process transaction must be committed");
							panic_any(payload);
						}
						DropBehavior::PanicSecondary(payload) => panic_any(payload),
					}
				}
			}

			#[derive(Debug)]
			struct Provider(Arc<Shared>);

			impl SpawnProvider for Provider {
				fn spawn(
					&self,
					_attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<ProviderProduct> {
					let paths = self.0.paths();
					let mut command = <$native_command>::new(std::env::current_exe()?);
					command
						.args([
							"--exact",
							concat!(stringify!($module), "::delayed_marker_process"),
							"--nocapture",
						])
						.env(MARKER_ENV, &paths.directory);
					let child = command.spawn()?;
					let child = Arc::new(Mutex::new(Box::new(child) as Box<dyn ChildWrapper>));
					*self.0.last_child.lock().unwrap() = Some(Arc::clone(&child));
					Ok(ProviderProduct::new(
						Box::new(SharedProcessChild {
							child: Arc::clone(&child),
						}),
						Box::new(Transaction {
							cleanup: Some(child),
							shared: Arc::clone(&self.0),
							committed: false,
							drop_behavior: self.0.take_drop_behavior(),
						}),
					))
				}
			}

			#[derive(Debug)]
			struct ProviderWrapper(Provider);

			impl CommandWrapper for ProviderWrapper {
				fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
					Some(&self.0)
				}
			}

			#[derive(Debug)]
			struct PrimaryError(Arc<()>);

			impl std::fmt::Display for PrimaryError {
				fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
					formatter.write_str("post-spawn failure")
				}
			}

			impl std::error::Error for PrimaryError {}

			#[derive(Debug)]
			struct FailPostSpawn(Mutex<Option<Arc<()>>>);

			impl CommandWrapper for FailPostSpawn {
				fn post_spawn(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_command: &CommandWrap,
				) -> io::Result<()> {
					self.0.lock().unwrap().take().map_or(Ok(()), |identity| {
						Err(io::Error::other(PrimaryError(identity)))
					})
				}
			}

			fn runtime() -> Option<tokio::runtime::Runtime> {
				$runtime
			}

			fn command(shared: Arc<Shared>) -> CommandWrap {
				let mut command = CommandWrap::new("provider-owned-program");
				command.wrap(ProviderWrapper(Provider(shared)));
				command
			}

			fn wait_for_path(path: &Path) {
				let deadline = Instant::now() + EXIT_TIMEOUT;
				while !path.exists() {
					assert!(
						Instant::now() < deadline,
						"path was not created: {}",
						path.display()
					);
					sleep(Duration::from_millis(5));
				}
			}

			fn reap_external(shared: &Shared) {
				let child = shared.child();
				let mut child = child
					.lock()
					.unwrap_or_else(std::sync::PoisonError::into_inner);
				let deadline = Instant::now() + EXIT_TIMEOUT;
				loop {
					if child.try_wait().unwrap().is_some() {
						return;
					}
					assert!(Instant::now() < deadline, "external child was not reaped");
					sleep(Duration::from_millis(5));
				}
			}

			#[test]
			fn delayed_marker_process() {
				let Some(directory) = std::env::var_os(MARKER_ENV) else {
					return;
				};
				let paths = MarkerPaths::new(Path::new(&directory));
				fs::write(&paths.ready, b"ready").expect("publish child readiness");
				let deadline = Instant::now() + EXIT_TIMEOUT;
				while !paths.go.exists() {
					if Instant::now() >= deadline {
						return;
					}
					sleep(Duration::from_millis(5));
				}
				fs::write(paths.marker, b"survived").expect("write delayed marker");
			}

			#[test]
			#[cfg_attr(miri, ignore = "requires native child processes")]
			fn committed_residue_destruction_occurs_after_a_controllable_child_is_returned() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let directory = tempfile::tempdir().unwrap();
				let paths = MarkerPaths::new(directory.path());
				let shared = Arc::new(Shared::default());
				shared.set_paths(paths.clone());
				let identity = Arc::new(());
				shared.set_drop_behavior(DropBehavior::PanicCommitted(DeferredPayload(
					Arc::clone(&identity),
				)));
				let mut command = command(Arc::clone(&shared));

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				let mut child = match outcome {
					Ok(Ok(child)) => child,
					Ok(Err(error)) => panic!("provider spawn failed: {error}"),
					Err(payload) => {
						let payload = payload
							.downcast::<DeferredPayload>()
							.expect("the committed transaction supplied the panic payload");
						assert!(Arc::ptr_eq(&payload.0, &identity));
						wait_for_path(&paths.ready);
						fs::write(&paths.go, b"go").unwrap();
						wait_for_path(&paths.marker);
						reap_external(&shared);
						shared.clear_child();
						panic!("spawn lost its child while the delayed marker process survived");
					}
				};

				wait_for_path(&paths.ready);
				child.start_kill().expect("start child termination");
				let first = $wait_for_child!(runtime, child).expect("await child termination");
				let second = $wait_for_child!(runtime, child).expect("repeat child wait");
				assert_eq!(first, second);
				assert_eq!(
					child.try_wait().expect("repeat child try_wait"),
					Some(first)
				);
				fs::write(&paths.go, b"go").unwrap();
				assert!(
					!paths.marker.exists(),
					"terminated child wrote its delayed marker"
				);

				let payload = catch_unwind(AssertUnwindSafe(|| drop(child)))
					.expect_err("child disposal destroys committed transaction residue");
				let payload = payload
					.downcast::<DeferredPayload>()
					.expect("the exact deferred transaction payload is preserved");
				assert!(Arc::ptr_eq(&payload.0, &identity));
				assert_eq!(shared.events(), vec![Event::Commit, Event::TransactionDrop]);
				assert!(!shared.events().contains(&Event::Rollback));
				assert!(
					shared
						.child()
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.try_wait()
						.unwrap()
						.is_some()
				);
				shared.clear_child();

				let reused_directory = tempfile::tempdir().unwrap();
				let reused_paths = MarkerPaths::new(reused_directory.path());
				shared.set_paths(reused_paths.clone());
				let mut child = command.spawn().expect("the command remains reusable");
				wait_for_path(&reused_paths.ready);
				child.start_kill().expect("start reused child termination");
				let first =
					$wait_for_child!(runtime, child).expect("await reused child termination");
				let second = $wait_for_child!(runtime, child).expect("repeat reused child wait");
				assert_eq!(first, second);
				fs::write(&reused_paths.go, b"go").unwrap();
				assert!(!reused_paths.marker.exists());
				drop(child);
				shared.clear_child();
				assert_eq!(
					shared.events(),
					vec![Event::Commit, Event::TransactionDrop, Event::Commit]
				);
			}

			#[test]
			#[cfg_attr(miri, ignore = "requires native child processes")]
			fn rollback_reaps_the_child_before_transaction_disposal_panics() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let directory = tempfile::tempdir().unwrap();
				let paths = MarkerPaths::new(directory.path());
				let shared = Arc::new(Shared::default());
				shared.set_paths(paths.clone());
				let secondary_drops = Arc::new(AtomicUsize::new(0));
				shared.set_drop_behavior(DropBehavior::PanicSecondary(PanickingDropPayload(
					Arc::clone(&secondary_drops),
				)));
				let identity = Arc::new(());
				let mut command = command(Arc::clone(&shared));
				command.wrap(FailPostSpawn(Mutex::new(Some(Arc::clone(&identity)))));

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				let error = outcome
					.expect("transaction disposal must not replace the primary error")
					.expect_err("the post-spawn hook must fail");
				let error = error
					.get_ref()
					.and_then(|error| error.downcast_ref::<PrimaryError>())
					.expect("the exact primary error payload is preserved");
				assert!(Arc::ptr_eq(&error.0, &identity));
				assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
				assert_eq!(
					shared.events(),
					vec![Event::Rollback, Event::TransactionDrop]
				);
				assert!(
					shared
						.child()
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.try_wait()
						.unwrap()
						.is_some()
				);
				fs::write(&paths.go, b"go").unwrap();
				assert!(
					!paths.marker.exists(),
					"rolled-back child wrote its delayed marker"
				);
				shared.clear_child();

				let reused_directory = tempfile::tempdir().unwrap();
				let reused_paths = MarkerPaths::new(reused_directory.path());
				shared.set_paths(reused_paths.clone());
				let mut child = command.spawn().expect("the command remains reusable");
				wait_for_path(&reused_paths.ready);
				child.start_kill().expect("start reused child termination");
				$wait_for_child!(runtime, child).expect("await reused child termination");
				fs::write(&reused_paths.go, b"go").unwrap();
				assert!(!reused_paths.marker.exists());
				drop(child);
				shared.clear_child();
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

#[cfg(feature = "std")]
provider_capability_tests!(
	std_provider_capabilities,
	process_wrap::std::CommandWrap,
	process_wrap::std::SpawnAttempt,
	process_wrap::std::CommandWrapper,
	process_wrap::std::ChildWrapper,
	process_wrap::std::ProviderProduct,
	process_wrap::std::SpawnProvider,
	std::process::Command,
	std::process::Child,
	std_wait_for_child,
	None
);

#[cfg(feature = "tokio1")]
provider_capability_tests!(
	tokio_provider_capabilities,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	process_wrap::tokio::ProviderProduct,
	process_wrap::tokio::SpawnProvider,
	tokio::process::Command,
	tokio::process::Child,
	tokio_wait_for_child,
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);

#[cfg(feature = "std")]
real_provider_tests!(
	std_process_frontend,
	process_wrap::std::CommandWrap,
	process_wrap::std::SpawnAttempt,
	process_wrap::std::CommandWrapper,
	process_wrap::std::ChildWrapper,
	process_wrap::std::ProviderProduct,
	process_wrap::std::SpawnProvider,
	std::process::Command,
	std_shared_process_methods,
	std_wait_for_child,
	None
);

#[cfg(feature = "tokio1")]
real_provider_tests!(
	tokio_process_frontend,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	process_wrap::tokio::ProviderProduct,
	process_wrap::tokio::SpawnProvider,
	tokio::process::Command,
	tokio_shared_process_methods,
	tokio_wait_for_child,
	Some(
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
	)
);
