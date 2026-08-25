//! Exact Win32 process-spawn intent preparation.
//!
//! This private model consumes tracked spawn-attempt state directly. It retains the WTF-16 data and
//! portable wrapper policy needed by the ConPTY backend's `CreateProcessW` call without committing
//! those implementation invariants to the public API.

use std::{ffi::OsStr, io};

use crate::WindowsSpawnPolicy;

use super::super::SpawnAttempt;
use command::{PreparedCommandLine, WideCString, prepare_command_line};
use environment::{PreparedEnvironment, prepare_environment};

pub(super) mod command;
pub(super) mod environment;

#[derive(Debug, Eq, PartialEq)]
struct PreparedWindowsCommand {
	application_name: WideCString,
	command_line: WideCString,
	environment: PreparedEnvironment,
	current_dir: Option<WideCString>,
	creation: WindowsSpawnPolicy,
}

fn prepare(attempt: &SpawnAttempt) -> io::Result<PreparedWindowsCommand> {
	prepare_with(attempt, |inherits, changes| {
		prepare_environment(inherits, changes)
	})
}

fn prepare_with<'a>(
	attempt: &'a SpawnAttempt,
	build_environment: impl FnOnce(
		bool,
		Box<dyn Iterator<Item = (&'a OsStr, Option<&'a OsStr>)> + 'a>,
	) -> io::Result<PreparedEnvironment>,
) -> io::Result<PreparedWindowsCommand> {
	let args = attempt.get_portable_args().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"Windows PTY preparation requires portable command arguments",
		)
	})?;
	let PreparedCommandLine {
		application_name,
		command_line,
	} = prepare_command_line(attempt.get_program(), args)?;
	let current_dir = attempt
		.get_current_dir()
		.map(|directory| WideCString::from_os(directory.as_os_str(), "PTY current directory"))
		.transpose()?;
	let inherits = attempt.inherits_environment().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"Windows PTY preparation requires portable environment state",
		)
	})?;
	let environment = build_environment(inherits, attempt.get_envs())?;

	Ok(PreparedWindowsCommand {
		application_name,
		command_line,
		environment,
		current_dir,
		creation: attempt.windows_spawn_policy(),
	})
}

#[cfg(test)]
mod tests {
	use std::{
		ffi::OsString,
		os::windows::ffi::OsStringExt,
		path::PathBuf,
		sync::{Arc, Mutex},
	};

	#[cfg(all(feature = "creation-flags", feature = "job-object"))]
	use windows::Win32::System::Threading::{
		CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
	};

	use super::*;
	#[cfg(feature = "creation-flags")]
	use crate::tokio::CreationFlags;
	#[cfg(feature = "job-object")]
	use crate::tokio::JobObject;
	#[cfg(feature = "kill-on-drop")]
	use crate::tokio::KillOnDrop;
	use crate::tokio::{Command, CommandWrapper, ProviderProduct, SpawnProvider};

	#[derive(Debug)]
	struct PortableWrapper;

	impl CommandWrapper for PortableWrapper {
		fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<()> {
			attempt.arg("from-wrapper");
			Ok(())
		}
	}

	#[derive(Debug)]
	struct CaptureProvider(Arc<Mutex<Option<io::Result<PreparedWindowsCommand>>>>);

	impl SpawnProvider for CaptureProvider {
		fn validate_attempt(&self, attempt: &SpawnAttempt, _command: &Command) -> io::Result<()> {
			*self
				.0
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(prepare(attempt));
			Err(io::Error::other("Windows command model captured"))
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
	struct CaptureModel(CaptureProvider);

	impl CommandWrapper for CaptureModel {
		fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
			Some(&self.0)
		}
	}

	fn prepare_command(command: &mut Command) -> io::Result<PreparedWindowsCommand> {
		let captured = Arc::new(Mutex::new(None));
		command.wrap(CaptureModel(CaptureProvider(Arc::clone(&captured))));
		let error = command.spawn().unwrap_err();
		assert_eq!(error.to_string(), "Windows command model captured");
		captured
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
			.expect("model validation records one preparation result")
	}

	fn terminated(value: &str) -> Vec<u16> {
		value.encode_utf16().chain([0]).collect()
	}

	fn environment_block(entries: &[(&str, &str)]) -> PreparedEnvironment {
		let mut block = Vec::new();
		for (key, value) in entries {
			block.extend(key.encode_utf16());
			block.push(b'=' as u16);
			block.extend(value.encode_utf16());
			block.push(0);
		}
		block.push(0);
		PreparedEnvironment::Block(block)
	}

	#[test]
	fn prepares_ordered_public_builder_intent_and_portable_wrappers() {
		let mut command = Command::new("tool");
		command
			.arg("two words")
			.raw_arg(r#"/D "literal""#)
			.args(["tail"])
			.env_clear()
			.envs([("Name", "child"), ("Second", "two")])
			.env_remove("gone")
			.wrap(PortableWrapper);

		let prepared = prepare_command(&mut command).unwrap();

		assert_eq!(
			prepared.command_line.as_units(),
			terminated(r#""tool" "two words" /D "literal" tail from-wrapper"#)
		);
		assert_eq!(
			prepared.environment,
			environment_block(&[("Name", "child"), ("Second", "two")])
		);
	}

	#[test]
	fn preserves_wtf16_current_directories() {
		let directory = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
		let mut command = Command::new("tool");
		command.current_dir(PathBuf::from(directory));

		let prepared = prepare_command(&mut command).unwrap();
		assert_eq!(
			prepared.current_dir.unwrap().as_units(),
			[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800, 0]
		);
	}

	#[test]
	fn rejects_current_directory_nuls_before_preparing_the_environment() {
		let directory = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0]);
		let mut command = Command::new("tool");
		command.current_dir(PathBuf::from(directory));

		let error = prepare_command(&mut command).unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
		assert_eq!(
			error.to_string(),
			"PTY current directory contains an embedded NUL"
		);
	}

	#[cfg(all(feature = "creation-flags", feature = "job-object"))]
	#[test]
	fn derives_job_policy_in_both_wrapper_orders() {
		let user_flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
		for reverse in [false, true] {
			let mut command = Command::new("tool");
			if reverse {
				command.wrap(JobObject).wrap(CreationFlags(user_flags));
			} else {
				command.wrap(CreationFlags(user_flags)).wrap(JobObject);
			}

			let policy = prepare_command(&mut command).unwrap().creation;
			assert_eq!(policy.user_creation_flags(), user_flags.0);
			assert_eq!(
				policy.spawn_creation_flags(),
				(user_flags | CREATE_SUSPENDED).0
			);
			assert!(!policy.is_explicitly_suspended());
			assert!(policy.has_job_object());
			assert!(policy.is_temporarily_suspended());
			assert!(!policy.kills_on_drop());
		}
	}

	#[cfg(all(feature = "creation-flags", feature = "job-object"))]
	#[test]
	fn preserves_explicit_suspension_for_job_assignment() {
		let user_flags = CREATE_NO_WINDOW | CREATE_SUSPENDED;
		let mut command = Command::new("tool");
		command.wrap(CreationFlags(user_flags)).wrap(JobObject);

		let policy = prepare_command(&mut command).unwrap().creation;
		assert_eq!(policy.user_creation_flags(), user_flags.0);
		assert_eq!(policy.spawn_creation_flags(), user_flags.0);
		assert!(policy.is_explicitly_suspended());
		assert!(policy.has_job_object());
		assert!(!policy.is_temporarily_suspended());
	}

	#[cfg(feature = "creation-flags")]
	#[test]
	fn preserves_creation_flags_without_a_job_object() {
		use windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

		let mut command = Command::new("tool");
		command.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP));

		let policy = prepare_command(&mut command).unwrap().creation;
		assert_eq!(policy.user_creation_flags(), CREATE_NEW_PROCESS_GROUP.0);
		assert_eq!(policy.spawn_creation_flags(), CREATE_NEW_PROCESS_GROUP.0);
		assert!(!policy.is_explicitly_suspended());
		assert!(!policy.has_job_object());
		assert!(!policy.is_temporarily_suspended());
	}

	#[cfg(feature = "job-object")]
	#[test]
	fn derives_temporary_suspension_for_a_job_without_creation_flags() {
		let mut command = Command::new("tool");
		command.wrap(JobObject);

		let policy = prepare_command(&mut command).unwrap().creation;
		assert_eq!(policy.user_creation_flags(), 0);
		assert_eq!(policy.spawn_creation_flags(), 0x0000_0004);
		assert!(!policy.is_explicitly_suspended());
		assert!(policy.has_job_object());
		assert!(policy.is_temporarily_suspended());
	}

	#[cfg(feature = "kill-on-drop")]
	#[test]
	fn records_kill_on_drop_without_a_job_object() {
		let mut command = Command::new("tool");
		command.wrap(KillOnDrop);

		let policy = prepare_command(&mut command).unwrap().creation;
		assert!(!policy.has_job_object());
		assert!(policy.kills_on_drop());
	}

	#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
	#[test]
	fn records_kill_on_drop_for_job_policy() {
		let mut command = Command::new("tool");
		command.wrap(JobObject).wrap(KillOnDrop);

		let policy = prepare_command(&mut command).unwrap().creation;
		assert!(policy.has_job_object());
		assert!(policy.kills_on_drop());
	}
}
