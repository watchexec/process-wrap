macro_rules! wrapper_lookup_tests {
	($module:ident, $command_wrap:path, $command_wrapper:path) => {
		mod $module {
			use $command_wrap as CommandWrap;
			use $command_wrapper as CommandWrapper;

			#[derive(Debug)]
			struct LookupWrapper {
				value: usize,
				extensions: usize,
			}

			impl CommandWrapper for LookupWrapper {
				fn extend(&mut self, other: Self) {
					self.value += other.value;
					self.extensions += other.extensions + 1;
				}
			}

			#[derive(Debug)]
			struct MissingWrapper;

			impl CommandWrapper for MissingWrapper {}

			fn command() -> CommandWrap {
				CommandWrap::with_new("", |_| {})
			}

			#[test]
			fn gets_stored_wrapper_by_concrete_type() {
				let mut command = command();
				assert!(!command.has_wrap::<LookupWrapper>());
				assert!(command.get_wrap::<LookupWrapper>().is_none());
				assert!(command.get_wrap::<MissingWrapper>().is_none());

				command.wrap(LookupWrapper {
					value: 42,
					extensions: 0,
				});

				assert!(command.has_wrap::<LookupWrapper>());
				let wrapper = command
					.get_wrap::<LookupWrapper>()
					.expect("the wrapper was just registered");
				assert_eq!(wrapper.value, 42);
				assert_eq!(wrapper.extensions, 0);
			}

			#[test]
			fn duplicate_type_extends_the_stored_wrapper() {
				let mut command = command();
				command
					.wrap(LookupWrapper {
						value: 1,
						extensions: 0,
					})
					.wrap(LookupWrapper {
						value: 2,
						extensions: 3,
					});

				let wrapper = command
					.get_wrap::<LookupWrapper>()
					.expect("the first wrapper remains registered");
				assert_eq!(wrapper.value, 3);
				assert_eq!(wrapper.extensions, 4);
			}

			#[test]
			fn command_wrap_is_sync() {
				fn assert_sync<T: Sync>() {}
				assert_sync::<CommandWrap>();
			}
		}
	};
}

#[cfg(feature = "std")]
wrapper_lookup_tests!(
	std_frontend,
	process_wrap::std::CommandWrap,
	process_wrap::std::CommandWrapper
);

#[cfg(feature = "tokio1")]
wrapper_lookup_tests!(
	tokio_frontend,
	process_wrap::tokio::CommandWrap,
	process_wrap::tokio::CommandWrapper
);
