#![cfg_attr(
	not(any(feature = "std", feature = "tokio1")),
	allow(unused_macros, unused_imports)
)]

macro_rules! Wrap {
    ($backend:ty, $command:ty, $child:ty, $childer:ident, $first_child_wrapper:expr) => {
        trait ErasedCommandWrapper: ::std::fmt::Debug + Send + Sync {
            fn as_command_wrapper_mut(&mut self) -> &mut dyn CommandWrapper;
            fn as_any(&self) -> &dyn ::std::any::Any;
            fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any;
        }

        impl<W: CommandWrapper + 'static> ErasedCommandWrapper for W {
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

        impl crate::command::Command<$backend> {
            fn wrapper_registry(&self) -> &WrapperRegistry {
                self.registry()
            }

            fn wrapper_registry_mut(&mut self) -> &mut WrapperRegistry {
                self.registry_mut()
            }

            /// Add a wrapper to the command.
            ///
            /// This is a lazy method, and the wrapper is not actually applied until `spawn` is
            /// called.
            ///
            /// Only one wrapper of a given type can be applied to a command. If `wrap` is called
            /// twice with the same type, the existing wrapper receives the newly registered wrapper
            /// through its typed `extend` hook and can merge its configuration. If the hook does
            /// nothing, the _new_ wrapper is silently discarded.
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
                invoke: impl FnOnce(&mut dyn CommandWrapper, &CommandWrap) -> ::std::io::Result<T>,
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

            #[inline]
            fn run_pre_spawn(&mut self, command: &mut $command) -> ::std::io::Result<()> {
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
                    self.with_wrapper_at(index, |wrapper, core| {
                        wrapper.pre_spawn(command, core)
                    })?;
                }

                Ok(())
            }

            #[inline]
            fn run_wrap_child(
                &mut self,
                mut child: Box<dyn $childer>,
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
                    child = self.with_wrapper_at(index, |wrapper, core| {
                        wrapper.wrap_child(child, core)
                    })?;
                }

                Ok(child)
            }

            #[inline]
            fn spawn_inner(
                &mut self,
                command: &mut $command,
                spawner: impl FnOnce(&mut $command) -> ::std::io::Result<$child>,
            ) -> ::std::io::Result<Box<dyn $childer>> {
                self.run_pre_spawn(command)?;

                let mut child = spawner(command)?;
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
                    self.with_wrapper_at(index, |wrapper, core| {
                        wrapper.post_spawn(command, &mut child, core)
                    })?;
                }

                let child = Box::new(
                    #[allow(clippy::redundant_closure_call)]
                    $first_child_wrapper(child),
                ) as Box<dyn $childer>;

                self.run_wrap_child(child)
            }

            #[inline]
            fn spawn_with_child_inner(
                &mut self,
                command: &mut $command,
                spawner: impl FnOnce(
                    &mut $command,
                ) -> ::std::io::Result<Box<dyn $childer>>,
            ) -> ::std::io::Result<Box<dyn $childer>> {
                self.run_pre_spawn(command)?;
                let child = spawner(command)?;
                self.run_wrap_child(child)
            }

            /// Spawn the command, returning a child that can be interacted with.
            ///
            /// In order, this runs all the `pre_spawn` hooks, then spawns the command, then runs
            /// all the `post_spawn` hooks, then stacks all the `wrap_child`s. As it returns a boxed
            /// trait object, only the methods from the trait are available directly; however you
            /// may downcast to the concrete type of the last applied wrapper if you need to.
            pub fn spawn(&mut self) -> ::std::io::Result<Box<dyn $childer>> {
                self.spawn_with(|command| command.spawn())
            }

            /// Spawn the command using a custom native-child spawner function.
            ///
            /// This is like [`spawn`](Self::spawn), but instead of calling `command.spawn()`
            /// directly, it calls the provided closure to create the native child process. This is
            /// useful when you need to use a platform-specific spawning mechanism that still returns
            #[doc = concat!("a [`", stringify!($child), "`].")]
            ///
            /// The lifecycle is the same as `spawn`: all `pre_spawn` hooks run first, then
            /// the provided closure is called, then `post_spawn` hooks, then `wrap_child`.
            pub fn spawn_with(
                &mut self,
                spawner: impl FnOnce(&mut $command) -> ::std::io::Result<$child>,
            ) -> ::std::io::Result<Box<dyn $childer>> {
                self.with_native(|core, command| core.spawn_inner(command, spawner))
            }

            /// Spawn the command using a custom boxed-child spawner function.
            ///
            /// This is the spawning path for custom child implementations which do not return the
            #[doc = concat!("native [`", stringify!($child), "`] type. The closure must return a boxed [`", stringify!($childer), "`] trait object.")]
            ///
            /// All `pre_spawn` hooks run first, then the provided closure is called, then
            /// `wrap_child` hooks are applied. `post_spawn` is intentionally skipped because that
            #[doc = concat!("hook requires a native [`", stringify!($child), "`]. Use [`spawn_with`](Self::spawn_with) when the spawner returns one.")]
            pub fn spawn_with_child(
                &mut self,
                spawner: impl FnOnce(
                    &mut $command,
                ) -> ::std::io::Result<Box<dyn $childer>>,
            ) -> ::std::io::Result<Box<dyn $childer>> {
                self.with_native(|core, command| {
                    core.spawn_with_child_inner(command, spawner)
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
            /// Returns `None` if the wrapper is not present. While a wrapper's lifecycle hook is
            /// running, that active wrapper remains registered but is temporarily unavailable through
            /// this method; peer wrappers remain available. To merely check registration, use
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
        /// This trait provides extension or hook points into the lifecycle of a command. See the
        /// [crate-level doc](crate) for an overview.
        ///
        /// All methods are optional, so a minimal impl may be:
        ///
        /// ```rust,ignore
        /// #[derive(Debug)]
        /// pub struct YourWrapper;
        #[doc = concat!("impl ", stringify!(CommandWrapper), " for YourWrapper {}\n```")]
        pub trait CommandWrapper: ::std::fmt::Debug + Send + Sync {
            /// Called on a first instance if a second of the same type is added.
            ///
            /// Only one wrapper of a given type can exist within a Wrap at a time. By default,
            /// later registrations are discarded. In some cases it is useful to merge their
            /// configuration instead. This method is called on the stored wrapper with the newly
            /// registered wrapper of the same concrete type.
            ///
            /// Because `other` is `Self`, implementations can inspect or move its type-specific
            /// fields directly without downcasting.
            ///
            /// Default impl: no-op.
            fn extend(&mut self, _other: Self)
            where
                Self: Sized,
            {
            }

            /// Called before the command is spawned, to mutate it as needed.
            ///
            /// This is where to modify the native command for one spawn attempt. It also gives mutable
            /// access to the wrapper instance, so state can be stored if needed. The `core`
            /// reference gives access to data from other wrappers; for example, that's how
            /// `CreationFlags` on Windows works along with `JobObject`.
            ///
            /// Default impl: no-op.
            fn pre_spawn(
                &mut self,
                _command: &mut $command,
                _core: &CommandWrap,
            ) -> ::std::io::Result<()> {
                Ok(())
            }

            /// Called after spawn, but before the child is wrapped.
            ///
            /// The `core` reference gives access to data from other wrappers; for example, that's
            /// how `CreationFlags` on Windows works along with `JobObject`.
            ///
            /// Default: no-op.
            fn post_spawn(
                &mut self,
                _command: &mut $command,
                _child: &mut $child,
                _core: &CommandWrap,
            ) -> ::std::io::Result<()> {
                Ok(())
            }

            /// Called to wrap a child into this command wrapper's child wrapper.
            ///
            /// If the wrapper needs to override the methods on Child, then it should create an
            /// instance of its own type implementing `ChildWrapper` and return it here. Child wraps
            /// are _in order_: you may end up with a `Foo(Bar(Child))` or a `Bar(Foo(Child))`
            /// depending on if `.wrap(Foo).wrap(Bar)` or `.wrap(Bar).wrap(Foo)` was called.
            ///
            /// The `core` reference gives access to data from other wrappers; for example, that's
            /// how `CreationFlags` on Windows works along with `JobObject`.
            ///
            /// Default: no-op (ie, returns the child unchanged).
            fn wrap_child(
                &mut self,
                child: Box<dyn $childer>,
                _core: &CommandWrap,
            ) -> ::std::io::Result<Box<dyn $childer>> {
                Ok(child)
            }
        }
    };
}

pub(crate) use Wrap;
