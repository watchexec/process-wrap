macro_rules! Wrap {
	($name:ident, $command:ty, $wrapper:ident, $child:ty, $childer:ident, $first_child_wrapper:expr) => {
		#[derive(Debug)]
		pub struct $name {
			command: $command,
			wrappers: ::indexmap::IndexMap<::std::any::TypeId, Box<dyn $wrapper>>,
		}

		impl $name {
			pub fn with_new(
				program: impl AsRef<::std::ffi::OsStr>,
				init: impl FnOnce(&mut $command),
			) -> Self {
				let mut command = <$command>::new(program);
				init(&mut command);
				Self {
					command,
					wrappers: ::indexmap::IndexMap::new(),
				}
			}

			pub fn wrap<W: $wrapper + 'static>(&mut self, wrapper: W) -> &mut Self {
				let typeid = ::std::any::TypeId::of::<W>();
				let mut wrapper = Some(Box::new(wrapper));
				let extant = self
					.wrappers
					.entry(typeid)
					.or_insert_with(|| wrapper.take().unwrap());
				if let Some(wrapper) = wrapper {
					extant.extend(wrapper);
				}

				self
			}

			// poor man's try..finally block
			#[inline]
			fn spawn_inner(
				&self,
				command: &mut $command,
				wrappers: &mut ::indexmap::IndexMap<::std::any::TypeId, Box<dyn $wrapper>>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				for (id, wrapper) in wrappers.iter_mut() {
					::tracing::debug!(?id, "pre_spawn");
					wrapper.pre_spawn(command, self)?;
				}

				let mut child = command.spawn()?;
				for (id, wrapper) in wrappers.iter_mut() {
					::tracing::debug!(?id, "post_spawn");
					wrapper.post_spawn(&mut child, self)?;
				}

				let mut child = Box::new(
					#[allow(clippy::redundant_closure_call)]
					$first_child_wrapper(child),
				) as Box<dyn $childer>;

				for (id, wrapper) in wrappers.iter_mut() {
					::tracing::debug!(?id, "wrap_child");
					child = wrapper.wrap_child(child, self)?;
				}

				Ok(child)
			}

			pub fn spawn(&mut self) -> ::std::io::Result<Box<dyn $childer>> {
				let mut command = ::std::mem::replace(&mut self.command, <$command>::new(""));
				let mut wrappers = ::std::mem::take(&mut self.wrappers);

				let res = self.spawn_inner(&mut command, &mut wrappers);

				self.command = command;
				self.wrappers = wrappers;

				res
			}

			pub fn has_wrap<W: $wrapper + 'static>(&self) -> bool {
				let typeid = ::std::any::TypeId::of::<W>();
				self.wrappers.contains_key(&typeid)
			}

			pub fn get_wrap<W: $wrapper + 'static>(&self) -> Option<&W> {
				let typeid = ::std::any::TypeId::of::<W>();
				self.wrappers.get(&typeid).map(|w| {
					let w_any = w as &dyn ::std::any::Any;
					w_any
						.downcast_ref()
						.expect("downcasting is guaranteed to succeed due to wrap()'s internals")
				})
			}
		}

		impl From<Command> for $name {
			fn from(command: $command) -> Self {
				Self {
					command,
					wrappers: ::indexmap::IndexMap::new(),
				}
			}
		}

		pub trait $wrapper: ::std::fmt::Debug {
			// process-wrap guarantees that `other` will be of the same type as `self`
			// note that other crates that may use this trait should guarantee this, but
			// that cannot be enforced by the type system, so you should still panic if
			// downcasting fails, instead of potentially causing UB
			fn extend(&mut self, _other: Box<dyn $wrapper>) {}

			fn pre_spawn(&mut self, _command: &mut $command, _core: &$name) -> Result<()> {
				Ok(())
			}

			fn post_spawn(&mut self, _child: &mut $child, _core: &$name) -> Result<()> {
				Ok(())
			}

			fn wrap_child(
				&mut self,
				child: Box<dyn $childer>,
				_core: &$name,
			) -> Result<Box<dyn $childer>> {
				Ok(child)
			}
		}
	};
}

pub(crate) use Wrap;
