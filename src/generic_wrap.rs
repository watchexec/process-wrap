#![cfg_attr(
	not(any(feature = "std", feature = "tokio1")),
	allow(unused_macros, unused_imports)
)]

macro_rules! Wrap {
	($backend:ty, $command:ty, $child:ty, $childer:ident, $first_child_wrapper:expr) => {
		trait ErasedCommandWrapper: ::std::fmt::Debug + Send + Sync {
			fn as_command_wrapper(&self) -> &dyn CommandWrapper;
			fn as_command_wrapper_mut(&mut self) -> &mut dyn CommandWrapper;
			fn as_any(&self) -> &dyn ::std::any::Any;
			fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any;
		}

		impl<W: CommandWrapper + 'static> ErasedCommandWrapper for W {
			fn as_command_wrapper(&self) -> &dyn CommandWrapper {
				self
			}

			fn as_command_wrapper_mut(&mut self) -> &mut dyn CommandWrapper {
				self
			}

			fn as_any(&self) -> &dyn ::std::any::Any {
				self
			}

			fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any {
				self
			}
		}

		#[derive(Debug, Default)]
		struct WrapperRegistry {
			wrappers: ::indexmap::IndexMap<
				::std::any::TypeId,
				Option<Box<dyn ErasedCommandWrapper>>,
			>,
		}

		impl crate::command::Backend for $backend {
			type NativeCommand = $command;

			fn new_registry() -> Box<dyn ::std::any::Any + Send + Sync> {
				Box::new(WrapperRegistry::default())
			}
		}

		/// A configurable process command with composable wrappers.
		pub type Command = crate::command::Command<$backend>;

		/// Backwards-compatible name for [`Command`].
		pub type CommandWrap = Command;

		/// The command configuration for one spawn attempt.
		pub type SpawnAttempt = crate::command::SpawnAttempt<$backend>;

		/// A child and armed cleanup transaction returned by a [`SpawnProvider`].
		///
		/// The child must implement the complete contract for this frontend's `ChildWrapper`, including
		/// any platform capabilities required by registered wrappers. The transaction must be fresh,
		/// armed, independently owned from the child chain, and able to undo this specific spawn until
		/// process-wrap commits it.
		#[derive(Debug)]
		pub struct ProviderProduct {
			child: Box<dyn $childer>,
			transaction: Box<dyn crate::SpawnTransaction>,
		}

		impl ProviderProduct {
			/// Create a provider product from its child and armed cleanup transaction.
			///
			/// Construct this only after the child has been created successfully. The transaction must own
			/// everything needed to terminate and reap that child and release provider resources if a later
			/// hook, child wrapper, or transaction commit fails or panics.
			pub fn new(
				child: Box<dyn $childer>,
				transaction: Box<dyn crate::SpawnTransaction>,
			) -> Self {
				Self { child, transaction }
			}

			fn into_parts(
				self,
			) -> (Box<dyn $childer>, Box<dyn crate::SpawnTransaction>) {
				(self.child, self.transaction)
			}
		}

		/// An alternate transport for spawning this frontend's child contract.
		///
		/// Providers are exposed by command wrappers through [`CommandWrapper::spawn_provider`]. A
		/// command may register only one provider. The same provider and wrapper instances are reused
		/// across repeated spawn attempts, so callbacks take `&self` and must not consume persistent
		/// configuration.
		///
		/// Process-wrap invokes provider callbacks in this order: `check_available`, native-only base
		/// rejection, `validate_command`, every `pre_spawn` hook in registration order, native-only
		/// attempt rejection, `validate_attempt`, and `spawn`. After `spawn` returns a product, every
		/// `post_spawn` and child-wrapping hook runs in registration order before process-wrap commits the
		/// product's transaction. `spawn_with` and `spawn_with_child` reject a registered provider instead
		/// of bypassing it.
		pub trait SpawnProvider: ::std::fmt::Debug + Send + Sync + 'static {
			/// Check whether this provider is available on the current platform and runtime.
			///
			/// This runs before command validation or native-only rejection so an unsupported provider
			/// retains error precedence. It must not allocate per-spawn operating-system resources.
			fn check_available(&self) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Validate immutable command and wrapper configuration before hooks run.
			///
			/// This must not allocate per-spawn operating-system resources. The provider-owning wrapper is
			/// registered but temporarily unavailable through `Command::get_wrap` during this callback;
			/// peer wrappers remain available.
			fn validate_command(&self, _command: &Command) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Validate the completed portable attempt before operating-system allocation.
			///
			/// Providers should inspect `get_portable_args`, `inherits_environment`, `get_envs`, the current
			/// directory, and platform policy getters here, then reject any policy they cannot preserve.
			/// Process-wrap has already rejected an opaque attempt before this callback.
			fn validate_attempt(
				&self,
				_attempt: &SpawnAttempt,
				_command: &Command,
			) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Spawn a child and return it with a fresh armed cleanup transaction.
			///
			/// The provider must honor every portable setting accepted by `validate_attempt`. Until this
			/// method returns a `ProviderProduct`, it remains responsible for cleaning up resources and any
			/// child it creates if it returns an error or panics.
			fn spawn(
				&self,
				attempt: &mut SpawnAttempt,
				command: &Command,
			) -> ::std::io::Result<ProviderProduct>;
		}

		impl crate::command::Command<$backend> {
			fn wrapper_registry(&self) -> &WrapperRegistry {
				self.registry()
			}

			fn wrapper_registry_mut(&mut self) -> &mut WrapperRegistry {
				self.registry_mut()
			}

			/// Add a wrapper to the command.
			///
			/// This is a lazy method, and the wrapper is not actually applied until `spawn` is called.
			///
			/// Only one wrapper of a given type can be applied to a command. If `wrap` is called twice
			/// with the same type, the existing wrapper receives the newly registered wrapper through
			/// its typed `extend` hook and can merge its configuration. If the hook does nothing, the
			/// _new_ wrapper is silently discarded.
			///
			/// Returns `&mut self` for chaining.
			pub fn wrap<W: CommandWrapper + 'static>(&mut self, wrapper: W) -> &mut Self {
				let typeid = ::std::any::TypeId::of::<W>();
				let mut wrapper = Some(wrapper);
				let extant = self
					.wrapper_registry_mut()
					.wrappers
					.entry(typeid)
					.or_insert_with(|| {
						Some(Box::new(wrapper.take().unwrap()) as Box<dyn ErasedCommandWrapper>)
					});
				if let Some(wrapper) = wrapper {
					extant
						.as_mut()
						.expect("wrap() cannot run while the matching wrapper's hook is active")
						.as_any_mut()
						.downcast_mut::<W>()
						.expect("downcasting is guaranteed to succeed due to wrap()'s internals")
						.extend(wrapper);
				}

				self
			}

			#[inline]
			fn with_wrapper_at<T>(
				&mut self,
				index: usize,
				invoke: impl FnOnce(&mut dyn CommandWrapper, &Command) -> ::std::io::Result<T>,
			) -> ::std::io::Result<T> {
				let mut wrapper = self
					.wrapper_registry_mut()
					.wrappers
					.get_index_mut(index)
					.expect("wrapper indices cannot disappear during ordered hook traversal")
					.1
					.take()
					.expect("each wrapper is present when its lifecycle hook begins");

				let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
					invoke(wrapper.as_command_wrapper_mut(), self)
				}));

				let slot = self
					.wrapper_registry_mut()
					.wrappers
					.get_index_mut(index)
					.expect("wrapper registrations cannot disappear while their hooks run")
					.1;
				debug_assert!(slot.is_none());
				*slot = Some(wrapper);

				match result {
					Ok(result) => result,
					Err(payload) => ::std::panic::resume_unwind(payload),
				}
			}

			fn select_spawn_provider(&self) -> ::std::io::Result<Option<usize>> {
				let mut selected = None;
				for (index, wrapper) in self.wrapper_registry().wrappers.values().enumerate() {
					let wrapper = wrapper
						.as_ref()
						.expect("provider selection runs outside wrapper lifecycle hooks");
					if wrapper
						.as_command_wrapper()
						.spawn_provider()
						.is_some()
					{
						if selected.replace(index).is_some() {
							return Err(::std::io::Error::new(
								::std::io::ErrorKind::InvalidInput,
								"multiple spawn providers are registered",
							));
						}
					}
				}
				Ok(selected)
			}

			fn with_spawn_provider_at<T>(
				&mut self,
				index: usize,
				invoke: impl FnOnce(&dyn SpawnProvider, &Command) -> ::std::io::Result<T>,
			) -> ::std::io::Result<T> {
				self.with_wrapper_at(index, |wrapper, command| {
					let provider = wrapper
						.spawn_provider()
						.expect("the selected wrapper continues to expose its spawn provider");
					invoke(provider, command)
				})
			}

			fn reject_explicit_provider(&self) -> ::std::io::Result<()> {
				if self.select_spawn_provider()?.is_some() {
					Err(::std::io::Error::new(
						::std::io::ErrorKind::InvalidInput,
						"an explicit spawner cannot bypass a registered spawn provider",
					))
				} else {
					Ok(())
				}
			}

			#[inline]
			fn run_pre_spawn(&mut self, attempt: &mut SpawnAttempt) -> ::std::io::Result<()> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					#[cfg(feature = "tracing")]
					{
						let id = self
							.wrapper_registry()
							.wrappers
							.get_index(index)
							.expect("wrapper indices cannot disappear during ordered hook traversal")
							.0;
						::tracing::debug!(?id, "pre_spawn");
					}
					self.with_wrapper_at(index, |wrapper, command| {
						wrapper.pre_spawn(attempt, command)
					})?;
				}

				Ok(())
			}

			#[cfg(windows)]
			#[inline]
			fn run_prepare_child(
				&mut self,
				attempt: &mut SpawnAttempt,
				child: &mut dyn $childer,
			) -> ::std::io::Result<
				Vec<Option<Box<dyn ::std::any::Any + Send>>>,
			> {
				let len = self.wrapper_registry().wrappers.len();
				let mut prepared = Vec::with_capacity(len);
				for index in 0..len {
					#[cfg(feature = "tracing")]
					{
						let id = self
							.wrapper_registry()
							.wrappers
							.get_index(index)
							.expect("wrapper indices cannot disappear during ordered hook traversal")
							.0;
						::tracing::debug!(?id, "prepare_child");
					}
					prepared.push(self.with_wrapper_at(index, |wrapper, command| {
						wrapper.prepare_child(attempt, child, command)
					})?);
				}

				Ok(prepared)
			}

			#[inline]
			fn run_post_spawn(
				&mut self,
				attempt: &mut SpawnAttempt,
				child: &mut dyn $childer,
			) -> ::std::io::Result<()> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					#[cfg(feature = "tracing")]
					{
						let id = self
							.wrapper_registry()
							.wrappers
							.get_index(index)
							.expect("wrapper indices cannot disappear during ordered hook traversal")
							.0;
						::tracing::debug!(?id, "post_spawn");
					}
					self.with_wrapper_at(index, |wrapper, command| {
						wrapper.post_spawn(attempt, child, command)
					})?;
				}

				Ok(())
			}

			#[inline]
			fn run_wrap_child(
				&mut self,
				mut child: Box<dyn $childer>,
				#[cfg(windows)] mut prepared: Vec<
					Option<Box<dyn ::std::any::Any + Send>>,
				>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					#[cfg(feature = "tracing")]
					{
						let id = self
							.wrapper_registry()
							.wrappers
							.get_index(index)
							.expect("wrapper indices cannot disappear during ordered hook traversal")
							.0;
						::tracing::debug!(?id, "wrap_child");
					}
					child = self.with_wrapper_at(index, |wrapper, command| {
						#[cfg(windows)]
						{
							wrapper.wrap_prepared_child(
								child,
								prepared[index].take(),
								command,
							)
						}
						#[cfg(not(windows))]
						{
							wrapper.wrap_child(child, command)
						}
					})?;
				}

				Ok(child)
			}

			fn finish_spawn(
				&mut self,
				attempt: &mut SpawnAttempt,
				mut child: Box<dyn $childer>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				#[cfg(windows)]
				let mut cleanup = if attempt.starts_suspended() {
					let handle = match child.as_ref().try_process_handle() {
						Some(handle) => handle,
						None => {
							let _ = child.start_kill();
							return Err(::std::io::Error::new(
								::std::io::ErrorKind::Unsupported,
								"child wrapper does not expose a Windows process handle",
							));
						}
					};
					match crate::command::WindowsSpawnCleanup::new(handle) {
						Ok(cleanup) => Some(cleanup),
						Err(error) => {
							let _ = crate::command::terminate_process_and_wait(handle);
							return Err(error);
						}
					}
				} else {
					None
				};

				let result = (|| {
					#[cfg(windows)]
					let prepared = self.run_prepare_child(attempt, child.as_mut())?;
					self.run_post_spawn(attempt, child.as_mut())?;
					#[cfg(windows)]
					{
						self.run_wrap_child(child, prepared)
					}
					#[cfg(not(windows))]
					{
						self.run_wrap_child(child)
					}
				})();
				#[cfg(windows)]
				let result = result.and_then(|mut child| {
					child.finalize_spawn()?;
					if let Some(cleanup) = cleanup.as_mut() {
						cleanup.disarm();
					}
					Ok(child)
				});
				result
			}

			fn rollback_transaction(transaction: Box<dyn crate::SpawnTransaction>) {
				let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
					let mut transaction = transaction;
					let _ = transaction.rollback();
				}));
			}

			fn finish_provider_spawn(
				&mut self,
				attempt: &mut SpawnAttempt,
				product: ProviderProduct,
			) -> ::std::io::Result<Box<dyn $childer>> {
				let (mut child, transaction) = product.into_parts();
				let mut transaction = Some(transaction);
				let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
					#[cfg(windows)]
					let prepared = self.run_prepare_child(attempt, child.as_mut())?;
					self.run_post_spawn(attempt, child.as_mut())?;
					#[cfg(windows)]
					let child = self.run_wrap_child(child, prepared)?;
					#[cfg(not(windows))]
					let child = self.run_wrap_child(child)?;
					transaction
						.as_mut()
						.expect("the provider transaction remains armed until commit")
						.commit()?;
					drop(
						transaction
							.take()
							.expect("a committed provider transaction is still present"),
					);
					#[cfg(windows)]
					let child = {
						let mut child = child;
						child.finalize_spawn()?;
						child
					};
					Ok(child)
				}));

				match result {
					Ok(Ok(child)) => Ok(child),
					Ok(Err(error)) => {
						if let Some(transaction) = transaction.take() {
							Self::rollback_transaction(transaction);
						}
						Err(error)
					}
					Err(payload) => {
						if let Some(transaction) = transaction.take() {
							Self::rollback_transaction(transaction);
						}
						::std::panic::resume_unwind(payload)
					}
				}
			}

			fn spawn_with_provider(
				&mut self,
				provider_index: usize,
			) -> ::std::io::Result<Box<dyn $childer>> {
				self.with_spawn_provider_at(provider_index, |provider, _| {
					provider.check_available()
				})?;
				if self.is_native_only() {
					return Err(::std::io::Error::new(
						::std::io::ErrorKind::InvalidInput,
						"a spawn provider cannot use a native-only command",
					));
				}
				self.with_spawn_provider_at(provider_index, |provider, command| {
					provider.validate_command(command)
				})?;

				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					if attempt.is_native_only() {
						return Err(::std::io::Error::new(
							::std::io::ErrorKind::InvalidInput,
							"a spawn provider cannot use a native-only spawn attempt",
						));
					}
					command.with_spawn_provider_at(provider_index, |provider, command| {
						provider.validate_attempt(attempt, command)
					})?;
					let product = command.with_spawn_provider_at(
						provider_index,
						|provider, command| provider.spawn(attempt, command),
					)?;
					command.finish_provider_spawn(attempt, product)
				})
			}

			/// Spawn the command, returning a child that can be interacted with.
			///
			/// With no alternate provider, this runs all `pre_spawn` hooks, spawns through the native
			/// frontend, runs all capability-level `post_spawn` hooks, then stacks all
			/// `wrap_child`s. A registered provider replaces only the native transport and commits its
			/// cleanup transaction after the same complete hook chain succeeds.
			pub fn spawn(&mut self) -> ::std::io::Result<Box<dyn $childer>> {
				if let Some(provider_index) = self.select_spawn_provider()? {
					return self.spawn_with_provider(provider_index);
				}

				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					let child = attempt.native_for_spawn().spawn()?;
					let child = Box::new(
						#[allow(clippy::redundant_closure_call)]
						$first_child_wrapper(child),
					) as Box<dyn $childer>;
					command.finish_spawn(attempt, child)
				})
			}

			/// Spawn the command using a custom native-child spawner function.
			///
			/// This explicit transport cannot be combined with a registered spawn provider. Without a
			/// provider, it runs the same pre-spawn, capability-level post-spawn, and child-wrapping
			/// lifecycle as [`spawn`](Self::spawn).
			///
			/// On Unix, replacing the native command is supported, but the displaced command must be
			/// dropped before the spawner returns. Retaining it prevents process-wrap from determining
			/// whether the callback carrying built-in child setup belongs to the current command.
			pub fn spawn_with(
				&mut self,
				spawner: impl FnOnce(&mut $command) -> ::std::io::Result<$child>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				self.reject_explicit_provider()?;
				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					let child = spawner(attempt.native_for_explicit_spawn())?;
					let child = Box::new(
						#[allow(clippy::redundant_closure_call)]
						$first_child_wrapper(child),
					) as Box<dyn $childer>;
					command.finish_spawn(attempt, child)
				})
			}

			/// Spawn the command using a custom boxed-child spawner function.
			///
			/// This is the spawning path for custom child implementations which do not return the
			#[doc = concat!("native [`", stringify!($child), "`] type. The closure must return a boxed [`", stringify!($childer), "`] trait object.")]
			///
			/// This explicit transport cannot be combined with a registered spawn provider. Without a
			/// provider, all `pre_spawn`, capability-level `post_spawn`, and `wrap_child` hooks run.
			///
			/// On Unix, replacing the native command is supported, but the displaced command must be
			/// dropped before the spawner returns. Retaining it prevents process-wrap from determining
			/// whether the callback carrying built-in child setup belongs to the current command.
			pub fn spawn_with_child(
				&mut self,
				spawner: impl FnOnce(
					&mut $command,
				) -> ::std::io::Result<Box<dyn $childer>>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				self.reject_explicit_provider()?;
				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					let child = spawner(attempt.native_for_explicit_spawn())?;
					command.finish_spawn(attempt, child)
				})
			}

			/// Check if a wrapper of a given type is present.
			pub fn has_wrap<W: CommandWrapper + 'static>(&self) -> bool {
				let typeid = ::std::any::TypeId::of::<W>();
				self.wrapper_registry().wrappers.contains_key(&typeid)
			}

			/// Get a reference to a wrapper of a given type.
			///
			/// This is useful for getting access to the state of a wrapper, generally from within
			/// another wrapper.
			///
			/// Returns `None` if the wrapper is not present. While a wrapper's lifecycle hook or provider
			/// callback is running, that active wrapper remains registered but is temporarily unavailable
			/// through this method; peer wrappers remain available. To merely check registration, use
			/// `has_wrap` instead.
			pub fn get_wrap<W: CommandWrapper + 'static>(&self) -> Option<&W> {
				let typeid = ::std::any::TypeId::of::<W>();
				self.wrapper_registry()
					.wrappers
					.get(&typeid)
					.and_then(Option::as_deref)
					.map(|wrapper| {
						wrapper
							.as_any()
							.downcast_ref()
							.expect("downcasting is guaranteed to succeed due to wrap()'s internals")
					})
			}
		}

		impl From<$command> for crate::command::Command<$backend> {
			fn from(command: $command) -> Self {
				Self::from_native(command)
			}
		}

		/// A trait for adding functionality to a command.
		///
		/// This trait provides extension and hook points into the lifecycle of a command. See the
		/// [crate-level documentation](crate) for an overview.
		///
		/// All methods are optional, so a minimal implementation may be:
		///
		/// ```rust,ignore
		/// #[derive(Debug)]
		/// pub struct YourWrapper;
		#[doc = concat!("impl ", stringify!(CommandWrapper), " for YourWrapper {}\n```")]
		pub trait CommandWrapper: ::std::fmt::Debug + Send + Sync {
			/// Called on a first instance if a second of the same type is added.
			///
			/// Only one wrapper of a given type can exist within a command at a time. By default, later
			/// registrations are discarded. In some cases it is useful to merge their configuration
			/// instead. This method is called on the stored wrapper with the newly registered wrapper of
			/// the same concrete type.
			///
			/// Because `other` is `Self`, implementations can inspect or move its type-specific fields
			/// directly without downcasting.
			///
			/// Default implementation: no-op.
			fn extend(&mut self, _other: Self)
			where
				Self: Sized,
			{
			}

			/// Called before the command is spawned, to mutate this attempt as needed.
			///
			/// Hooks run in registration order and stop at the first error or panic. Mutations to an attempt
			/// copied from a tracked command apply to that spawn only. A native-only base instead retains
			/// native mutations when process-wrap restores it after the lifecycle.
			///
			/// Calling `SpawnAttempt::native_mut`, directly or through `stdin`, `stdout`, or `stderr`, makes a
			/// tracked attempt opaque. A registered portable provider rejects it after all pre-spawn hooks
			/// and before `validate_attempt` or operating-system allocation. Portable policy setters remain
			/// representable; a transport may apply their policy only after every hook has run and in the
			/// order required by the platform. On Unix, recurring native escapes from a native-only base can
			/// retain inactive dispatcher callbacks because the native API does not expose callback insertion
			/// or command ownership; use portable attempt methods for recurring configuration.
			///
			/// The `command` reference provides read-only access to peer wrappers and persistent base
			/// configuration. The active wrapper remains registered but is temporarily unavailable through
			/// `Command::get_wrap`.
			///
			/// Default implementation: no-op.
			fn pre_spawn(
				&mut self,
				_attempt: &mut SpawnAttempt,
				_command: &Command,
			) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Prepare Windows child state which must exist before public post-spawn hooks run.
			///
			/// Process-wrap retains the returned state through post-spawn hooks and supplies it to the
			/// matching wrapper's `wrap_prepared_child` call.
			#[doc(hidden)]
			#[cfg(windows)]
			fn prepare_child(
				&mut self,
				_attempt: &mut SpawnAttempt,
				_child: &mut dyn $childer,
				_command: &Command,
			) -> ::std::io::Result<Option<Box<dyn ::std::any::Any + Send>>> {
				Ok(None)
			}

			/// Called after any transport spawns a child, but before the child is wrapped.
			///
			/// Hooks run in registration order and stop at the first error or panic. The child is exposed
			/// through the frontend's object-safe capability trait, so it may be a terminal custom or
			/// provider child with no native child value. The transport has already created it: changing
			/// command settings on `attempt` here cannot configure that child.
			///
			/// On the provider path, an error or panic triggers best-effort transaction rollback. Native
			/// transports do not promise equivalent child cleanup on every platform.
			///
			/// Default implementation: no-op.
			fn post_spawn(
				&mut self,
				_attempt: &mut SpawnAttempt,
				_child: &mut dyn $childer,
				_command: &Command,
			) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Called to wrap a child into this command wrapper's child wrapper.
			///
			/// If the wrapper needs to override methods on the child, it should create an instance of its
			/// own type implementing `ChildWrapper` and return it here. Wrappers run in registration order
			/// and stop at the first error or panic, so `.wrap(Foo).wrap(Bar)` produces an outer
			/// `Bar(Foo(child))` layer. On the provider path, an error or panic triggers best-effort
			/// transaction rollback.
			///
			/// Default implementation: no-op (returns the child unchanged).
			fn wrap_child(
				&mut self,
				child: Box<dyn $childer>,
				_command: &Command,
			) -> ::std::io::Result<Box<dyn $childer>> {
				Ok(child)
			}

			/// Install a child wrapper using state returned by `prepare_child`.
			#[doc(hidden)]
			#[cfg(windows)]
			fn wrap_prepared_child(
				&mut self,
				child: Box<dyn $childer>,
				prepared: Option<Box<dyn ::std::any::Any + Send>>,
				command: &Command,
			) -> ::std::io::Result<Box<dyn $childer>> {
				debug_assert!(
					prepared.is_none(),
					"the default child preparation does not produce wrapper state"
				);
				self.wrap_child(child, command)
			}

			/// Expose an alternate spawn provider implemented by this wrapper.
			///
			/// If this returns `Some` during provider selection, it must continue returning `Some` for every
			/// callback in that spawn lifecycle. The returned provider must refer to the same persistent
			/// provider state. Only one registered wrapper may expose a provider.
			///
			/// During a provider callback, this owning wrapper remains registered but is temporarily
			/// unavailable through `Command::get_wrap`; peer wrappers remain available.
			///
			/// Default implementation: no provider.
			fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
				None
			}
		}
	};
}

pub(crate) use Wrap;
