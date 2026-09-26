#![cfg(all(any(feature = "std", feature = "tokio1"), any(unix, windows)))]

macro_rules! spawn_provider_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$child_wrapper_layer:path,
		$child_wrapper_slots:path,
		$pending_child_wrapper:path,
		$prepared_child:path,
		$provider_product:path,
		$spawn_provider:path,
		$runtime:expr
	) => {
		mod $module {
			use std::{
				any::TypeId,
				ffi::OsStr,
				io::{self, Read},
				panic::{AssertUnwindSafe, catch_unwind, panic_any},
				process::{ExitStatus, Output, Stdio},
				sync::{
					Arc, Mutex,
					atomic::{AtomicBool, AtomicUsize, Ordering},
					mpsc,
				},
				thread::sleep,
				time::{Duration, Instant},
			};

			use process_wrap::{CommandArg, SpawnTransaction};
			use $child_wrapper as ChildWrapper;
			#[cfg(windows)]
			use $child_wrapper_layer as ChildWrapperLayer;
			#[cfg(windows)]
			use $child_wrapper_slots as ChildWrapperSlots;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $pending_child_wrapper as PendingChildWrapper;
			#[cfg(windows)]
			use $prepared_child as PreparedChild;
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
				ChildDrop,
				#[cfg(windows)]
				PreparedDrop(&'static str),
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
				child_drop_payload: Mutex<Option<PanickingDropPayload>>,
				commit_primary_failure: Mutex<Option<PrimaryFailure>>,
				#[cfg(windows)]
				owner_primary_failure: Mutex<Option<PrimaryFailure>>,
				transaction_debug_calls: AtomicUsize,
				panic_in_transaction_debug: AtomicBool,
				make_attempt_native_only: AtomicBool,
				#[cfg(windows)]
				use_resume_child: AtomicBool,
				#[cfg(windows)]
				resume_calls: AtomicUsize,
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

				fn set_child_drop_payload(&self, payload: PanickingDropPayload) {
					*self.child_drop_payload.lock().unwrap() = Some(payload);
				}

				fn take_child_drop_payload(&self) -> Option<PanickingDropPayload> {
					self.child_drop_payload.lock().unwrap().take()
				}

				fn set_commit_primary_failure(&self, failure: PrimaryFailure) {
					*self.commit_primary_failure.lock().unwrap() = Some(failure);
				}

				fn take_commit_primary_failure(&self) -> Option<PrimaryFailure> {
					self.commit_primary_failure.lock().unwrap().take()
				}

				#[cfg(windows)]
				fn set_owner_primary_failure(&self, failure: PrimaryFailure) {
					*self.owner_primary_failure.lock().unwrap() = Some(failure);
				}

				#[cfg(windows)]
				fn take_owner_primary_failure(&self) -> Option<PrimaryFailure> {
					self.owner_primary_failure.lock().unwrap().take()
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
			struct ResumeChild(Arc<Shared>);

			#[cfg(windows)]
			impl ChildWrapper for ResumeChild {
				fn inner(&self) -> &dyn ChildWrapper {
					self
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self
				}

				fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
					self
				}

				fn resume_after_job_assignment(&mut self) -> Option<io::Result<()>> {
					self.0.resume_calls.fetch_add(1, Ordering::SeqCst);
					Some(Ok(()))
				}

				fn start_kill(&mut self) -> io::Result<()> {
					Ok(())
				}

				fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
					Ok(Some(successful_exit_status()))
				}
			}

			#[derive(Debug)]
			struct PanickingChild {
				shared: Arc<Shared>,
				payload: Option<PanickingDropPayload>,
			}

			impl ChildWrapper for PanickingChild {
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

			impl Drop for PanickingChild {
				fn drop(&mut self) {
					self.shared.event(Event::ChildDrop);
					panic_any(
						self.payload
							.take()
							.expect("the child destructor panics only once"),
					);
				}
			}

			fn assert_provider_child(child: &dyn ChildWrapper) {
				let id = child.type_id();
				let expected = id == TypeId::of::<CustomChild>() || id == TypeId::of::<PanickingChild>();
				#[cfg(windows)]
				let expected = expected || id == TypeId::of::<ResumeChild>();
				assert!(expected);
			}

			#[cfg(windows)]
			#[derive(Debug)]
			struct PanickingPrepared {
				shared: Arc<Shared>,
				name: &'static str,
				payload: Option<PanickingDropPayload>,
			}

			#[cfg(windows)]
			impl Drop for PanickingPrepared {
				fn drop(&mut self) {
					self.shared.event(Event::PreparedDrop(self.name));
					panic_any(
						self.payload
							.take()
							.expect("the prepared-state destructor panics only once"),
					);
				}
			}

			#[cfg(windows)]
			#[derive(Debug)]
			struct PreparedLayer {
				inner: Option<Box<dyn ChildWrapper>>,
				prepared: Option<PreparedChild>,
			}

			#[cfg(windows)]
			impl PreparedLayer {
				fn detached() -> Self {
					Self {
						inner: None,
						prepared: None,
					}
				}
			}

			#[cfg(windows)]
			impl ChildWrapperLayer for PreparedLayer {
				fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
					ChildWrapperSlots::new(&mut self.inner).with_prepared(&mut self.prepared)
				}
			}

			#[cfg(windows)]
			impl ChildWrapper for PreparedLayer {
				fn inner(&self) -> &dyn ChildWrapper {
					self.inner
						.as_deref()
						.expect("an installed prepared layer owns its child")
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.inner
						.as_deref_mut()
						.expect("an installed prepared layer owns its child")
				}

				fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.inner
						.take()
						.expect("an installed prepared layer owns its child")
				}
			}

			#[cfg(windows)]
			macro_rules! prepared_wrapper {
				($name:ident, $label:literal) => {
					#[derive(Debug)]
					struct $name {
						shared: Arc<Shared>,
						payload: Mutex<Option<PanickingDropPayload>>,
					}

					impl CommandWrapper for $name {
						fn prepare_child(
							&mut self,
							_attempt: &mut SpawnAttempt,
							_child: &mut dyn ChildWrapper,
							_command: &CommandWrap,
						) -> io::Result<Option<Box<dyn std::any::Any + Send>>> {
							Ok(self.payload.lock().unwrap().take().map(|payload| {
								Box::new(PanickingPrepared {
									shared: Arc::clone(&self.shared),
									name: $label,
									payload: Some(payload),
								}) as Box<dyn std::any::Any + Send>
							}))
						}

						fn wrap_prepared_child(
							&mut self,
							_child: &mut dyn ChildWrapper,
							prepared: Option<&PreparedChild>,
							_command: &CommandWrap,
						) -> io::Result<Option<PendingChildWrapper>> {
							Ok(prepared.map(|_| PendingChildWrapper::new(PreparedLayer::detached())))
						}
					}
				};
			}

			#[cfg(windows)]
			prepared_wrapper!(FirstPrepared, "first");
			#[cfg(windows)]
			prepared_wrapper!(SecondPrepared, "second");

			#[cfg(windows)]
			#[derive(Debug)]
			struct FailPrepare(Mutex<Option<PrimaryFailure>>);

			#[cfg(windows)]
			impl CommandWrapper for FailPrepare {
				fn prepare_child(
					&mut self,
					_attempt: &mut SpawnAttempt,
					_child: &mut dyn ChildWrapper,
					_command: &CommandWrap,
				) -> io::Result<Option<Box<dyn std::any::Any + Send>>> {
					let failure = {
						self.0
							.lock()
							.unwrap_or_else(std::sync::PoisonError::into_inner)
							.take()
					};
					match failure {
						Some(PrimaryFailure::Error(identity)) => {
							Err(io::Error::other(IdentityError(identity)))
						}
						Some(PrimaryFailure::Panic(identity)) => panic_any(PrimaryPanic(identity)),
						None => Ok(None),
					}
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
						if let Some(failure) = self.shared.take_owner_primary_failure() {
							match failure {
								PrimaryFailure::Error(identity) => {
									return Err(io::Error::other(IdentityError(identity)));
								}
								PrimaryFailure::Panic(identity) => panic_any(PrimaryPanic(identity)),
							}
						}
						self.armed = false;
					} else {
						self.shared.event(Event::DisarmNonOwner(self.name));
						self.shared.fail(Point::DisarmNonOwner)?;
					}
					Ok(())
				}
			}

			#[cfg(windows)]
			impl FinalizationChild {
				fn child_slot(&mut self) -> &mut Option<Box<dyn ChildWrapper>> {
					if self.inner.is_none() {
						return &mut self.inner;
					}
					let inner = self
						.inner
						.as_deref_mut()
						.expect("a prebuilt finalization chain has an inner layer");
					let inner = (inner as &mut dyn std::any::Any)
						.downcast_mut::<Self>()
						.expect("a detached finalization chain contains only finalization layers");
					inner.child_slot()
				}
			}

			#[cfg(windows)]
			impl ChildWrapperLayer for FinalizationChild {
				fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
					ChildWrapperSlots::new(self.child_slot())
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
			fn finalization_layer_chain(shared: Arc<Shared>) -> Option<FinalizationChild> {
				shared
					.finalization_layers()
					.into_iter()
					.rev()
					.fold(None, |inner, layer| {
						Some(FinalizationChild {
							inner: inner.map(|inner| Box::new(inner) as Box<dyn ChildWrapper>),
							shared: Arc::clone(&shared),
							name: layer.name,
							owns_job_cleanup: layer.owns_job_cleanup,
							armed: layer.owns_job_cleanup,
						})
					})
			}

			struct Transaction {
				shared: Arc<Shared>,
				committed: bool,
			}

			impl std::fmt::Debug for Transaction {
				fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
					self.shared.transaction_debug_calls.fetch_add(1, Ordering::SeqCst);
					if self
						.shared
						.panic_in_transaction_debug
						.load(Ordering::SeqCst)
					{
						panic_any("transaction Debug must remain private");
					}
					formatter.debug_struct("Transaction").finish_non_exhaustive()
				}
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
					if let Some(failure) = self.shared.take_commit_primary_failure() {
						match failure {
							PrimaryFailure::Error(identity) => {
								return Err(io::Error::other(IdentityError(identity)));
							}
							PrimaryFailure::Panic(identity) => panic_any(PrimaryPanic(identity)),
						}
					}
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
					let child: Box<dyn ChildWrapper> =
						if let Some(payload) = self.shared.take_child_drop_payload() {
							Box::new(PanickingChild {
								shared: Arc::clone(&self.shared),
								payload: Some(payload),
							})
						} else {
							#[cfg(windows)]
							if self.shared.use_resume_child.swap(false, Ordering::SeqCst) {
								Box::new(ResumeChild(Arc::clone(&self.shared)))
							} else {
								Box::new(CustomChild)
							}
							#[cfg(not(windows))]
							Box::new(CustomChild)
						};
					Ok(ProviderProduct::new(
						child,
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
					assert_provider_child(child);
					self.shared().event(Event::Post(self.name));
					self.shared().fail(Point::Post)
				}

				fn wrap_child(
					&mut self,
					child: &mut dyn ChildWrapper,
					command: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					self.assert_hook_visibility(command);
					assert_provider_child(child);
					self.shared().event(Event::Wrap(self.name));
					self.shared().fail(Point::Wrap)?;
					Ok(None)
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
						assert_provider_child(child);
					}
					self.shared.event(Event::Post("peer"));
					self.shared.fail(Point::PeerPost)
				}

				fn wrap_child(
					&mut self,
					child: &mut dyn ChildWrapper,
					command: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					self.assert_hook_visibility(command);
					if self.expect_custom_child {
						assert_provider_child(child);
					}
					self.shared.event(Event::Wrap("peer"));
					self.shared.fail(Point::PeerWrap)?;
					#[cfg(windows)]
					return Ok(finalization_layer_chain(Arc::clone(&self.shared))
						.map(PendingChildWrapper::new));
					#[cfg(not(windows))]
					Ok(None)
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
					let payload = match outcome {
						Err(payload) => payload,
						Ok(_) => panic!("the primary panic must be resumed"),
					};
					let payload = match payload.downcast::<PrimaryPanic>() {
						Ok(payload) => payload,
						Err(secondary) => {
							std::mem::forget(secondary);
							panic!("cleanup replaced the primary panic payload");
						}
					};
					assert!(Arc::ptr_eq(&payload.0, identity));
				} else {
					let result = match outcome {
						Ok(result) => result,
						Err(secondary) => {
							std::mem::forget(secondary);
							panic!("cleanup replaced the primary error with a panic");
						}
					};
					let error = result.expect_err("the primary error must be returned");
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

			fn bounded_test_process(
				test_name: &str,
				environment: (&str, &str),
				timeout: Duration,
			) -> (Output, bool) {
				const REAP_TIMEOUT: Duration = Duration::from_secs(5);

				struct ChildGuard(Option<std::process::Child>);

				impl ChildGuard {
					fn terminate_and_reap(&mut self) -> io::Result<ExitStatus> {
						let child = self
							.0
							.as_mut()
							.expect("the guarded subprocess remains available");
						match child.kill() {
							Ok(()) => {}
							Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
							Err(error) => return Err(error),
						}
						let deadline = Instant::now() + REAP_TIMEOUT;
						loop {
							if let Some(status) = child.try_wait()? {
								return Ok(status);
							}
							if Instant::now() >= deadline {
								return Err(io::Error::new(
									io::ErrorKind::TimedOut,
									"isolated lifecycle subprocess was not reaped after termination",
								));
							}
							sleep(Duration::from_millis(5));
						}
					}
				}

				impl Drop for ChildGuard {
					fn drop(&mut self) {
						if self.0.is_some() {
							let _ = self.terminate_and_reap();
						}
					}
				}

				fn drain_pipe(
					mut pipe: impl Read + Send + 'static,
				) -> mpsc::Receiver<io::Result<Vec<u8>>> {
					let (sender, receiver) = mpsc::channel();
					std::thread::spawn(move || {
						let mut bytes = Vec::new();
						let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
						let _ = sender.send(result);
					});
					receiver
				}

				fn collect_output(
					status: ExitStatus,
					stdout: mpsc::Receiver<io::Result<Vec<u8>>>,
					stderr: mpsc::Receiver<io::Result<Vec<u8>>>,
				) -> Output {
					let deadline = Instant::now() + REAP_TIMEOUT;
					let receive = |receiver: mpsc::Receiver<io::Result<Vec<u8>>>, label| {
						let remaining = deadline.saturating_duration_since(Instant::now());
						receiver
							.recv_timeout(remaining)
							.unwrap_or_else(|error| panic!("did not drain isolated {label}: {error}"))
							.unwrap_or_else(|error| panic!("could not read isolated {label}: {error}"))
					};
					Output {
						status,
						stdout: receive(stdout, "stdout"),
						stderr: receive(stderr, "stderr"),
					}
				}

				let mut spawned = std::process::Command::new(std::env::current_exe().unwrap())
					.args(["--exact", test_name, "--nocapture"])
					.env(environment.0, environment.1)
					.stdout(Stdio::piped())
					.stderr(Stdio::piped())
					.spawn()
					.expect("start isolated lifecycle regression");
				let stdout = drain_pipe(spawned.stdout.take().expect("capture isolated stdout"));
				let stderr = drain_pipe(spawned.stderr.take().expect("capture isolated stderr"));
				let mut child = ChildGuard(Some(spawned));
				let deadline = Instant::now() + timeout;
				loop {
					let status = child
						.0
						.as_mut()
						.expect("the subprocess remains guarded while polling")
						.try_wait()
						.expect("poll isolated lifecycle regression");
					if let Some(status) = status {
						child.0.take();
						return (collect_output(status, stdout, stderr), false);
					}
					if Instant::now() >= deadline {
						let status = child
							.terminate_and_reap()
							.expect("terminate and reap expired lifecycle subprocess");
						child.0.take();
						return (collect_output(status, stdout, stderr), true);
					}
					sleep(Duration::from_millis(5));
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

			#[cfg(windows)]
			#[test]
			fn committed_sidecar_delegates_post_transfer_exact_resume() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared.use_resume_child.store(true, Ordering::SeqCst);
				let mut command = provider_command(Arc::clone(&shared), "provider");

				let mut child = command.spawn().expect("spawn provider child");
				assert_eq!(shared.resume_calls.load(Ordering::SeqCst), 0);
				child
					.resume_after_job_assignment()
					.expect("the sidecar delegates an exact resume capability")
					.expect("the delegated exact resume succeeds");
				assert_eq!(shared.resume_calls.load(Ordering::SeqCst), 1);
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
			fn committed_sidecar_debug_never_formats_transaction_residue() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				shared
					.panic_in_transaction_debug
					.store(true, Ordering::SeqCst);
				let mut command = provider_command(Arc::clone(&shared), "provider");
				let child = command.spawn().expect("spawn provider child");

				let formatted = catch_unwind(AssertUnwindSafe(|| format!("{child:?}")))
					.expect("formatting a provider child must not inspect its transaction residue");
				assert!(formatted.contains("CustomChild"));
				assert_eq!(shared.transaction_debug_calls.load(Ordering::SeqCst), 0);
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
			fn bounded_subprocess_reaps_the_timeout_path() {
				const CHILD_ENV: &str = "PROCESS_WRAP_PROVIDER_TIMEOUT_HELPER";
				let child_value = stringify!($module);
				if std::env::var_os(CHILD_ENV).as_deref() == Some(OsStr::new(child_value)) {
					sleep(Duration::from_secs(30));
					return;
				}

				let (output, timed_out) = bounded_test_process(
					concat!(stringify!($module), "::bounded_subprocess_reaps_the_timeout_path"),
					(CHILD_ENV, child_value),
					Duration::ZERO,
				);
				assert!(timed_out, "the zero-deadline seam must take the timeout branch");
				assert!(!output.status.success(), "the timed-out helper must be terminated");
			}

			#[test]
			fn rollback_and_transaction_unwinds_are_separated() {
				const CHILD_ENV: &str = "PROCESS_WRAP_SEPARATE_PROVIDER_UNWINDS";
				let child_value = stringify!($module);
				if std::env::var_os(CHILD_ENV).as_deref() != Some(OsStr::new(child_value)) {
					let (output, timed_out) = bounded_test_process(
						concat!(stringify!($module), "::rollback_and_transaction_unwinds_are_separated"),
						(CHILD_ENV, child_value),
						EXIT_TIMEOUT,
					);
					assert!(!timed_out, "isolated lifecycle regression exceeded its deadline");
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
			fn child_destructor_panics_do_not_replace_post_spawn_or_commit_errors() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				for fail_commit in [false, true] {
					let shared = Arc::new(Shared::default());
					let secondary_drops = Arc::new(AtomicUsize::new(0));
					shared.set_child_drop_payload(PanickingDropPayload {
						drops: Arc::clone(&secondary_drops),
					});
					let identity = Arc::new(());
					let mut command = if fail_commit {
						shared.set_commit_primary_failure(PrimaryFailure::Error(Arc::clone(&identity)));
						provider_command(Arc::clone(&shared), "provider")
					} else {
						provider_command_with_primary_failure(
							Arc::clone(&shared),
							PrimaryFailure::Error(Arc::clone(&identity)),
						)
					};

					let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
					assert_primary_failure_preserved(outcome, &identity, false);
					assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
					assert!(shared.events().ends_with(&[Event::Rollback, Event::ChildDrop]));

					shared.clear_events();
					drop(command.spawn().expect("the command remains reusable"));
					assert_eq!(shared.events(), successful_events("provider"));
				}
			}

			#[test]
			fn child_destructor_panics_do_not_replace_post_spawn_or_commit_panics() {
				const CHILD_ENV: &str = "PROCESS_WRAP_PROVIDER_CHILD_PANIC";
				let module = stringify!($module);
				let selected = std::env::var(CHILD_ENV).ok();
				if selected.as_deref().is_none_or(|value| !value.starts_with(module)) {
					for phase in ["post", "commit"] {
						let child_value = format!("{module}:{phase}");
						let (output, timed_out) = bounded_test_process(
							concat!(
								stringify!($module),
								"::child_destructor_panics_do_not_replace_post_spawn_or_commit_panics"
							),
							(CHILD_ENV, &child_value),
							EXIT_TIMEOUT,
						);
						assert!(!timed_out, "isolated child cleanup exceeded its deadline");
						assert!(
							output.status.success(),
							"{phase} panic was not preserved across child cleanup:\nstdout:\n{}\nstderr:\n{}",
							String::from_utf8_lossy(&output.stdout),
							String::from_utf8_lossy(&output.stderr),
						);
					}
					return;
				}

				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let fail_commit = selected
					.as_deref()
					.expect("the subprocess selected a phase")
					.ends_with(":commit");
				let shared = Arc::new(Shared::default());
				let secondary_drops = Arc::new(AtomicUsize::new(0));
				shared.set_child_drop_payload(PanickingDropPayload {
					drops: Arc::clone(&secondary_drops),
				});
				let identity = Arc::new(());
				let mut command = if fail_commit {
					shared.set_commit_primary_failure(PrimaryFailure::Panic(Arc::clone(&identity)));
					provider_command(Arc::clone(&shared), "provider")
				} else {
					provider_command_with_primary_failure(
						Arc::clone(&shared),
						PrimaryFailure::Panic(Arc::clone(&identity)),
					)
				};

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				assert_primary_failure_preserved(outcome, &identity, true);
				assert_eq!(secondary_drops.load(Ordering::SeqCst), 0);
				assert!(shared.events().ends_with(&[Event::Rollback, Event::ChildDrop]));
				shared.clear_events();
				drop(command.spawn().expect("the command remains reusable"));
				assert_eq!(shared.events(), successful_events("provider"));
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
			fn prepared_states_are_disposed_independently_after_the_primary_is_owned() {
				const CHILD_ENV: &str = "PROCESS_WRAP_PROVIDER_PREPARED_PANIC";
				let module = stringify!($module);
				let selected = std::env::var(CHILD_ENV).ok();
				if selected.as_deref().is_none_or(|value| !value.starts_with(module)) {
					for failure in ["error", "panic"] {
						let child_value = format!("{module}:{failure}");
						let (output, timed_out) = bounded_test_process(
							concat!(
								stringify!($module),
								"::prepared_states_are_disposed_independently_after_the_primary_is_owned"
							),
							(CHILD_ENV, &child_value),
							EXIT_TIMEOUT,
						);
						assert!(!timed_out, "prepared-state cleanup exceeded its deadline");
						assert!(
							output.status.success(),
							"prepared-state cleanup did not preserve the {failure}:\nstdout:\n{}\nstderr:\n{}",
							String::from_utf8_lossy(&output.stdout),
							String::from_utf8_lossy(&output.stderr),
						);
					}
					return;
				}

				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let first_drops = Arc::new(AtomicUsize::new(0));
				let second_drops = Arc::new(AtomicUsize::new(0));
				let identity = Arc::new(());
				let failure = if selected
					.as_deref()
					.expect("the subprocess selected a failure")
					.ends_with(":panic")
				{
					PrimaryFailure::Panic(Arc::clone(&identity))
				} else {
					PrimaryFailure::Error(Arc::clone(&identity))
				};
				let was_panic = matches!(&failure, PrimaryFailure::Panic(_));
				let mut command = provider_command(Arc::clone(&shared), "provider");
				command
					.wrap(FirstPrepared {
						shared: Arc::clone(&shared),
						payload: Mutex::new(Some(PanickingDropPayload {
							drops: Arc::clone(&first_drops),
						})),
					})
					.wrap(SecondPrepared {
						shared: Arc::clone(&shared),
						payload: Mutex::new(Some(PanickingDropPayload {
							drops: Arc::clone(&second_drops),
						})),
					})
					.wrap(FailPrepare(Mutex::new(Some(failure))));

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				assert_primary_failure_preserved(outcome, &identity, was_panic);
				assert_eq!(first_drops.load(Ordering::SeqCst), 0);
				assert_eq!(second_drops.load(Ordering::SeqCst), 0);
				assert!(shared.events().ends_with(&[
					Event::Rollback,
					Event::PreparedDrop("first"),
					Event::PreparedDrop("second"),
				]));

				shared.clear_events();
				drop(command.spawn().expect("the command remains reusable"));
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
				assert!(events.contains(&Event::Rollback));
				assert!(events.contains(&Event::DisarmNonOwner("nonowner")));
				assert!(events.ends_with(&[
					Event::Rollback,
					Event::OwnerDropped("outer-owner", true),
					Event::OwnerDropped("inner-owner", true),
				]));
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
						Event::TransactionDrop,
						Event::OwnerDropped("owner", true),
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
			fn final_owner_failure_survives_child_prepared_and_residue_panics() {
				const CHILD_ENV: &str = "PROCESS_WRAP_PROVIDER_FINAL_OWNER_CLEANUP_PANICS";
				let module = stringify!($module);
				let selected = std::env::var(CHILD_ENV).ok();
				if selected.as_deref().is_none_or(|value| !value.starts_with(module)) {
					for failure in ["error", "panic"] {
						let child_value = format!("{module}:{failure}");
						let (output, timed_out) = bounded_test_process(
							concat!(
								stringify!($module),
								"::final_owner_failure_survives_child_prepared_and_residue_panics"
							),
							(CHILD_ENV, &child_value),
							EXIT_TIMEOUT,
						);
						assert!(!timed_out, "final-owner cleanup exceeded its deadline");
						assert!(
							output.status.success(),
							"cleanup replaced the final-owner {failure}:\nstdout:\n{}\nstderr:\n{}",
							String::from_utf8_lossy(&output.stdout),
							String::from_utf8_lossy(&output.stderr),
						);
					}
					return;
				}

				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let shared = Arc::new(Shared::default());
				let child_payload_drops = Arc::new(AtomicUsize::new(0));
				let prepared_payload_drops = Arc::new(AtomicUsize::new(0));
				let residue_payload_drops = Arc::new(AtomicUsize::new(0));
				shared.set_child_drop_payload(PanickingDropPayload {
					drops: Arc::clone(&child_payload_drops),
				});
				shared.set_transaction_drop_behavior(TransactionDropBehavior::PanicSecondary(
					PanickingDropPayload {
						drops: Arc::clone(&residue_payload_drops),
					},
				));
				let layers = vec![finalization_layer("owner", true)];
				shared.set_finalization_layers(layers.clone());
				let identity = Arc::new(());
				let failure = if selected
					.as_deref()
					.expect("the subprocess selected a failure")
					.ends_with(":panic")
				{
					PrimaryFailure::Panic(Arc::clone(&identity))
				} else {
					PrimaryFailure::Error(Arc::clone(&identity))
				};
				let was_panic = matches!(&failure, PrimaryFailure::Panic(_));
				shared.set_owner_primary_failure(failure);
				let mut command = provider_command(Arc::clone(&shared), "provider");
				command.wrap(FirstPrepared {
					shared: Arc::clone(&shared),
					payload: Mutex::new(Some(PanickingDropPayload {
						drops: Arc::clone(&prepared_payload_drops),
					})),
				});

				let outcome = catch_unwind(AssertUnwindSafe(|| command.spawn()));
				assert_primary_failure_preserved(outcome, &identity, was_panic);
				let events = shared.events();
				assert_eq!(events.iter().filter(|event| **event == Event::Commit).count(), 1);
				assert!(!events.contains(&Event::Rollback));
				assert!(events.ends_with(&[
					Event::Commit,
					Event::DisarmOwner("owner"),
					Event::TransactionDrop,
					Event::OwnerDropped("owner", true),
					Event::ChildDrop,
					Event::PreparedDrop("first"),
				]));
				assert_eq!(child_payload_drops.load(Ordering::SeqCst), 0);
				assert_eq!(prepared_payload_drops.load(Ordering::SeqCst), 0);
				assert_eq!(residue_payload_drops.load(Ordering::SeqCst), 0);

				shared.clear_events();
				let child = command.spawn().expect("the command remains reusable");
				drop(child);
				let mut expected = successful_finalization_events("provider", &layers);
				expected.push(Event::OwnerDropped("owner", false));
				assert_eq!(shared.events(), expected);
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

#[cfg(feature = "std")]
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

#[cfg(feature = "tokio1")]
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

#[cfg(feature = "std")]
macro_rules! std_wait_for_child {
	($runtime:expr, $child:expr) => {{
		let _ = &$runtime;
		$child.wait()
	}};
}

#[cfg(feature = "tokio1")]
macro_rules! tokio_wait_for_child {
	($runtime:expr, $child:expr) => {
		$runtime
			.as_ref()
			.expect("the Tokio frontend has a runtime")
			.block_on($child.wait())
	};
}

#[cfg(feature = "std")]
macro_rules! std_kill_child {
	($runtime:expr, $child:expr) => {{
		let _ = &$runtime;
		$child.kill()
	}};
}

#[cfg(feature = "tokio1")]
macro_rules! tokio_kill_child {
	($runtime:expr, $child:expr) => {
		$runtime
			.as_ref()
			.expect("the Tokio frontend has a runtime")
			.block_on(async { Box::into_pin($child.kill()).await })
	};
}

macro_rules! provider_capability_tests {
	(
		$module:ident,
		$command_wrap:path,
		$spawn_attempt:path,
		$command_wrapper:path,
		$child_wrapper:path,
		$child_wrapper_layer:path,
		$child_wrapper_slots:path,
		$pending_child_wrapper:path,
		$provider_product:path,
		$spawn_provider:path,
		$native_command:path,
		$native_child:path,
		$wait_for_child:ident,
		$kill_child:ident,
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
			use $child_wrapper_layer as ChildWrapperLayer;
			use $child_wrapper_slots as ChildWrapperSlots;
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;
			use $pending_child_wrapper as PendingChildWrapper;
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
				wait_for_completion: bool,
			}

			impl SpawnProvider for Provider {
				fn spawn(
					&self,
					_attempt: &mut SpawnAttempt,
					_command: &CommandWrap,
				) -> io::Result<ProviderProduct> {
					let mut command = <$native_command>::new(std::env::current_exe()?);
					let child_test = if self.wait_for_completion {
						concat!(stringify!($module), "::completed_child_process")
					} else {
						concat!(stringify!($module), "::live_child_process")
					};
					command
						.args(["--exact", child_test, "--nocapture"])
						.stdin(Stdio::piped())
						.stdout(Stdio::piped())
						.stderr(Stdio::piped());
					if !self.wait_for_completion {
						command.env("PROCESS_WRAP_LIVE_CAPABILITY_CHILD", "1");
					}
					let mut child = command.spawn()?;
					if self.wait_for_completion {
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
			struct OuterLayer(Option<Box<dyn ChildWrapper>>);

			impl ChildWrapperLayer for OuterLayer {
				fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
					ChildWrapperSlots::new(&mut self.0)
				}
			}

			impl ChildWrapper for OuterLayer {
				fn inner(&self) -> &dyn ChildWrapper {
					self.0
						.as_deref()
						.expect("an installed outer layer owns its child")
				}

				fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
					self.0
						.as_deref_mut()
						.expect("an installed outer layer owns its child")
				}

				fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
					self.0
						.take()
						.expect("an installed outer layer owns its child")
				}

				#[cfg(windows)]
				fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
					self.inner().process_handle()
				}
			}

			#[derive(Debug)]
			struct OuterWrapper;

			impl CommandWrapper for OuterWrapper {
				fn wrap_child(
					&mut self,
					_child: &mut dyn ChildWrapper,
					_command: &CommandWrap,
				) -> io::Result<Option<PendingChildWrapper>> {
					Ok(Some(PendingChildWrapper::new(OuterLayer(None))))
				}
			}

			fn runtime() -> Option<tokio::runtime::Runtime> {
				$runtime
			}

			fn command_with_completion(
				drops: Arc<AtomicUsize>,
				wait_for_completion: bool,
			) -> CommandWrap {
				let mut command = CommandWrap::new("provider-owned-program");
				command
					.wrap(ProviderWrapper(Provider {
						drops,
						wait_for_completion,
					}))
					.wrap(OuterWrapper);
				command
			}

			fn command(drops: Arc<AtomicUsize>) -> CommandWrap {
				command_with_completion(drops, true)
			}

			#[test]
			fn completed_child_process() {}

			#[test]
			fn live_child_process() {
				if std::env::var_os("PROCESS_WRAP_LIVE_CAPABILITY_CHILD").is_some() {
					sleep(Duration::from_secs(30));
				}
			}

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
				drop(child);
				assert_eq!(drops.load(Ordering::SeqCst), 1);

				let child = command.spawn().expect("spawn child for native extraction");
				// SAFETY: the process is complete, the transaction has no cleanup resources after commit,
				// and the forwarding layer owns no supervision state.
				let child = unsafe { child.try_into_inner_child() };
				let mut child = child.expect("extract native child");
				assert_eq!(drops.load(Ordering::SeqCst), 2);
				let first = $wait_for_child!(runtime, child).expect("wait extracted native child");
				let second = $wait_for_child!(runtime, child).expect("repeat extracted child wait");
				assert_eq!(first, second);
			}

			#[test]
			#[cfg_attr(miri, ignore = "requires native child processes")]
			fn committed_sidecar_delegates_direct_kill_and_repeated_waits() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let drops = Arc::new(AtomicUsize::new(0));
				let mut command = command_with_completion(Arc::clone(&drops), false);

				let mut child = command.spawn().expect("spawn live provider child");
				$kill_child!(runtime, child).expect("kill and reap live provider child");
				let first = $wait_for_child!(runtime, child).expect("repeat killed child wait");
				let second = $wait_for_child!(runtime, child).expect("second killed child wait");
				assert_eq!(first, second);
				assert_eq!(
					child.try_wait().expect("repeat killed child try_wait"),
					Some(first)
				);
				drop(child);
				assert_eq!(drops.load(Ordering::SeqCst), 1);
			}

			#[cfg(windows)]
			#[test]
			#[cfg_attr(miri, ignore = "requires native child processes")]
			fn committed_sidecar_delegates_a_live_process_handle() {
				let runtime = runtime();
				let _runtime_guard = runtime.as_ref().map(tokio::runtime::Runtime::enter);
				let drops = Arc::new(AtomicUsize::new(0));
				let mut command = command_with_completion(Arc::clone(&drops), false);

				let mut child = command.spawn().expect("spawn live provider child");
				assert_eq!(drops.load(Ordering::SeqCst), 0);
				assert!(child.try_process_handle().is_some());
				child.start_kill().expect("terminate live provider child");
				let first = $wait_for_child!(runtime, child).expect("reap live provider child");
				let second = $wait_for_child!(runtime, child).expect("repeat live child wait");
				assert_eq!(first, second);
				assert_eq!(
					child.try_wait().expect("repeat live child try_wait"),
					Some(first)
				);
				drop(child);
				assert_eq!(drops.load(Ordering::SeqCst), 1);
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
	process_wrap::std::ChildWrapperLayer,
	process_wrap::std::ChildWrapperSlots,
	process_wrap::std::PendingChildWrapper,
	process_wrap::std::PreparedChild,
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
	process_wrap::tokio::ChildWrapperLayer,
	process_wrap::tokio::ChildWrapperSlots,
	process_wrap::tokio::PendingChildWrapper,
	process_wrap::tokio::PreparedChild,
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
	process_wrap::std::ChildWrapperLayer,
	process_wrap::std::ChildWrapperSlots,
	process_wrap::std::PendingChildWrapper,
	process_wrap::std::ProviderProduct,
	process_wrap::std::SpawnProvider,
	std::process::Command,
	std::process::Child,
	std_wait_for_child,
	std_kill_child,
	None
);

#[cfg(feature = "tokio1")]
provider_capability_tests!(
	tokio_provider_capabilities,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::SpawnAttempt,
	process_wrap::tokio::CommandWrapper,
	process_wrap::tokio::ChildWrapper,
	process_wrap::tokio::ChildWrapperLayer,
	process_wrap::tokio::ChildWrapperSlots,
	process_wrap::tokio::PendingChildWrapper,
	process_wrap::tokio::ProviderProduct,
	process_wrap::tokio::SpawnProvider,
	tokio::process::Command,
	tokio::process::Child,
	tokio_wait_for_child,
	tokio_kill_child,
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
