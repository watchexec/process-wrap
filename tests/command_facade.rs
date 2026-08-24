#[cfg(feature = "std")]
mod std_frontend {
	use std::{ffi::OsStr, io};

	use process_wrap::std::{Command, CommandWrap, CommandWrapper};

	#[derive(Debug)]
	struct AttemptArgument;

	impl CommandWrapper for AttemptArgument {
		fn pre_spawn(
			&mut self,
			command: &mut std::process::Command,
			_core: &CommandWrap,
		) -> io::Result<()> {
			command.arg("attempt");
			Ok(())
		}
	}

	#[derive(Debug)]
	struct Marker;

	impl CommandWrapper for Marker {}

	#[test]
	fn inferred_with_new_uses_process_wrap_command() {
		let mut command = Command::with_new("tool", |command| {
			command.arg("first").env("MODE", "tracked");
		});
		command.command_mut().arg("second");

		assert_eq!(command.command().get_program(), OsStr::new("tool"));
		assert_eq!(
			command.get_args().collect::<Vec<_>>(),
			[OsStr::new("first"), OsStr::new("second")]
		);

		let alias: CommandWrap = command;
		assert_eq!(alias.get_program(), OsStr::new("tool"));
	}

	#[test]
	fn into_command_discards_wrappers() {
		let mut command = Command::new("tool");
		command.wrap(Marker);
		assert!(command.has_wrap::<Marker>());

		let command = command.into_command();
		assert!(!command.has_wrap::<Marker>());
	}

	#[test]
	fn tracked_attempt_mutations_do_not_accumulate() {
		let mut command = Command::with_new("tool", |command| {
			command.arg("base");
		});
		command.wrap(AttemptArgument);

		for _ in 0..2 {
			let error = command
				.spawn_with(|native| {
					assert_eq!(
						native.get_args().collect::<Vec<_>>(),
						[OsStr::new("base"), OsStr::new("attempt")]
					);
					native.arg("one-off");
					Err(io::Error::other("expected test error"))
				})
				.unwrap_err();
			assert_eq!(error.to_string(), "expected test error");
		}

		assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("base")]);
	}

	#[test]
	fn native_only_commands_preserve_exact_mutations() {
		let mut native = std::process::Command::new("tool");
		native.arg("native");
		let mut command = Command::from(native);
		command.arg("facade");
		command.native_mut().arg("escape");

		command
			.spawn_with(|native| {
				assert_eq!(
					native.get_args().collect::<Vec<_>>(),
					[
						OsStr::new("native"),
						OsStr::new("facade"),
						OsStr::new("escape")
					]
				);
				native.arg("persisted-attempt");
				Err(io::Error::other("expected test error"))
			})
			.unwrap_err();

		command
			.spawn_with(|native| {
				assert_eq!(
					native.get_args().collect::<Vec<_>>(),
					[
						OsStr::new("native"),
						OsStr::new("facade"),
						OsStr::new("escape"),
						OsStr::new("persisted-attempt")
					]
				);
				Err(io::Error::other("expected test error"))
			})
			.unwrap_err();
	}

	#[test]
	fn tracked_environment_and_cwd_materialize_exactly() {
		let cwd = std::env::current_dir().unwrap();
		let mut command = Command::new("tool");
		command
			.env("PROCESS_WRAP_REMOVED", "before-clear")
			.env_clear()
			.env("PROCESS_WRAP_PRESENT", "after-clear")
			.env_remove("PROCESS_WRAP_ABSENT")
			.current_dir(&cwd);

		command
			.spawn_with(|native| {
				let env = native.get_envs().collect::<Vec<_>>();
				assert!(
					!env.iter()
						.any(|(key, _)| *key == OsStr::new("PROCESS_WRAP_REMOVED"))
				);
				assert!(env.iter().any(|(key, value)| {
					*key == OsStr::new("PROCESS_WRAP_PRESENT")
						&& *value == Some(OsStr::new("after-clear"))
				}));
				assert!(
					!env.iter()
						.any(|(key, _)| *key == OsStr::new("PROCESS_WRAP_ABSENT"))
				);
				assert_eq!(native.get_current_dir(), Some(cwd.as_path()));
				Err(io::Error::other("expected test error"))
			})
			.unwrap_err();
	}

	#[cfg(windows)]
	#[test]
	fn tracked_raw_arguments_preserve_order() {
		let mut command = Command::new("tool");
		command.arg("regular-1").raw_arg(" raw ").arg("regular-2");

		assert_eq!(
			command.get_args().collect::<Vec<_>>(),
			[
				OsStr::new("regular-1"),
				OsStr::new(" raw "),
				OsStr::new("regular-2")
			]
		);
	}
}

#[cfg(feature = "tokio1")]
mod tokio_frontend {
	use std::{ffi::OsStr, io};

	use process_wrap::tokio::{Command, CommandWrap, CommandWrapper};

	#[derive(Debug)]
	struct AttemptArgument;

	impl CommandWrapper for AttemptArgument {
		fn pre_spawn(
			&mut self,
			command: &mut tokio::process::Command,
			_core: &CommandWrap,
		) -> io::Result<()> {
			command.arg("attempt");
			Ok(())
		}
	}

	#[derive(Debug)]
	struct Marker;

	impl CommandWrapper for Marker {}

	#[test]
	fn inferred_with_new_uses_process_wrap_command() {
		let mut command = Command::with_new("tool", |command| {
			command.arg("first").env("MODE", "tracked");
		});
		command.command_mut().arg("second");

		assert_eq!(command.command().get_program(), OsStr::new("tool"));
		assert_eq!(
			command.get_args().collect::<Vec<_>>(),
			[OsStr::new("first"), OsStr::new("second")]
		);

		let alias: CommandWrap = command;
		assert_eq!(alias.get_program(), OsStr::new("tool"));
	}

	#[test]
	fn into_command_discards_wrappers() {
		let mut command = Command::new("tool");
		command.wrap(Marker);
		assert!(command.has_wrap::<Marker>());

		let command = command.into_command();
		assert!(!command.has_wrap::<Marker>());
	}

	#[test]
	fn tracked_attempt_mutations_do_not_accumulate() {
		let mut command = Command::with_new("tool", |command| {
			command.arg("base");
		});
		command.wrap(AttemptArgument);

		for _ in 0..2 {
			let error = command
				.spawn_with(|native| {
					assert_eq!(
						native.as_std().get_args().collect::<Vec<_>>(),
						[OsStr::new("base"), OsStr::new("attempt")]
					);
					native.arg("one-off");
					Err(io::Error::other("expected test error"))
				})
				.unwrap_err();
			assert_eq!(error.to_string(), "expected test error");
		}

		assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("base")]);
	}

	#[test]
	fn native_only_commands_preserve_exact_mutations() {
		let mut native = tokio::process::Command::new("tool");
		native.arg("native");
		let mut command = Command::from(native);
		command.arg("facade");
		command.native_mut().arg("escape");

		command
			.spawn_with(|native| {
				assert_eq!(
					native.as_std().get_args().collect::<Vec<_>>(),
					[
						OsStr::new("native"),
						OsStr::new("facade"),
						OsStr::new("escape")
					]
				);
				native.arg("persisted-attempt");
				Err(io::Error::other("expected test error"))
			})
			.unwrap_err();

		command
			.spawn_with(|native| {
				assert_eq!(
					native.as_std().get_args().collect::<Vec<_>>(),
					[
						OsStr::new("native"),
						OsStr::new("facade"),
						OsStr::new("escape"),
						OsStr::new("persisted-attempt")
					]
				);
				Err(io::Error::other("expected test error"))
			})
			.unwrap_err();
	}

	#[test]
	fn tracked_environment_and_cwd_materialize_exactly() {
		let cwd = std::env::current_dir().unwrap();
		let mut command = Command::new("tool");
		command
			.env("PROCESS_WRAP_REMOVED", "before-clear")
			.env_clear()
			.env("PROCESS_WRAP_PRESENT", "after-clear")
			.env_remove("PROCESS_WRAP_ABSENT")
			.current_dir(&cwd);

		command
			.spawn_with(|native| {
				let env = native.as_std().get_envs().collect::<Vec<_>>();
				assert!(
					!env.iter()
						.any(|(key, _)| *key == OsStr::new("PROCESS_WRAP_REMOVED"))
				);
				assert!(env.iter().any(|(key, value)| {
					*key == OsStr::new("PROCESS_WRAP_PRESENT")
						&& *value == Some(OsStr::new("after-clear"))
				}));
				assert!(
					!env.iter()
						.any(|(key, _)| *key == OsStr::new("PROCESS_WRAP_ABSENT"))
				);
				assert_eq!(native.as_std().get_current_dir(), Some(cwd.as_path()));
				Err(io::Error::other("expected test error"))
			})
			.unwrap_err();
	}

	#[cfg(windows)]
	#[test]
	fn tracked_raw_arguments_preserve_order() {
		let mut command = Command::new("tool");
		command.arg("regular-1").raw_arg(" raw ").arg("regular-2");

		assert_eq!(
			command.get_args().collect::<Vec<_>>(),
			[
				OsStr::new("regular-1"),
				OsStr::new(" raw "),
				OsStr::new("regular-2")
			]
		);
	}
}

#[cfg(all(feature = "std", feature = "tokio1"))]
#[test]
fn both_frontend_aliases_coexist() {
	let blocking = process_wrap::std::Command::new("blocking");
	let asynchronous = process_wrap::tokio::Command::new("asynchronous");

	assert_eq!(blocking.get_program(), std::ffi::OsStr::new("blocking"));
	assert_eq!(
		asynchronous.get_program(),
		std::ffi::OsStr::new("asynchronous")
	);
}
