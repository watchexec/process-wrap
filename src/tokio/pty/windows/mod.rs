//! Exact Win32 process-spawn intent prepared from [`PtyCommand`].
//!
//! This private model never reconstructs arguments or environment operations from a Tokio command.
//! It retains the WTF-16 data and wrapper policy needed by the ConPTY backend's `CreateProcessW`
//! call without committing those implementation invariants to the public API.

use std::{any::TypeId, io};

use windows::Win32::System::Threading::{CREATE_SUSPENDED, PROCESS_CREATION_FLAGS};

#[cfg(feature = "creation-flags")]
use super::super::CreationFlags;
#[cfg(feature = "job-object")]
use super::super::JobObject;
#[cfg(feature = "kill-on-drop")]
use super::super::KillOnDrop;
#[cfg(feature = "job-object")]
use super::ChildWrapper;
use super::{EnvironmentIntent, PtyCommand, WrapperRegistration};
#[cfg(feature = "job-object")]
use crate::windows::job_creation_flags;
use command::{PreparedCommandLine, WideCString, prepare_command_line};
use environment::{PreparedEnvironment, prepare_environment};

mod api;
mod attributes;
mod child;
pub(super) mod command;
mod console;
pub(super) mod environment;
mod pipe;
mod program;
mod spawn;

#[derive(Debug, Eq, PartialEq)]
struct PreparedWindowsCommand {
	application_name: WideCString,
	command_line: WideCString,
	environment: PreparedEnvironment,
	current_dir: Option<WideCString>,
	creation: WindowsCreationPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsCreationPolicy {
	user_flags: PROCESS_CREATION_FLAGS,
	spawn_flags: PROCESS_CREATION_FLAGS,
	explicit_suspension: bool,
	has_job_object: bool,
	resume_after_assignment: bool,
	kill_on_drop: bool,
}

#[cfg(feature = "job-object")]
pub(super) fn try_resume_primary_thread(child: &mut dyn ChildWrapper) -> Option<io::Result<()>> {
	child
		.downcast_mut::<child::ConPtyChild>()
		.map(child::ConPtyChild::resume_primary_thread)
}

fn prepare(command: &PtyCommand) -> io::Result<PreparedWindowsCommand> {
	let mut prepared = prepare_with(command, prepare_environment)?;
	prepared.application_name = program::resolve(&command.intent)?;
	Ok(prepared)
}

fn prepare_with(
	command: &PtyCommand,
	build_environment: impl FnOnce(&EnvironmentIntent) -> io::Result<PreparedEnvironment>,
) -> io::Result<PreparedWindowsCommand> {
	validate_wrappers(&command.wrappers)?;
	let PreparedCommandLine {
		application_name,
		command_line,
	} = prepare_command_line(&command.intent)?;
	let current_dir = command
		.intent
		.current_dir
		.as_deref()
		.map(|directory| WideCString::from_os(directory, "PTY current directory"))
		.transpose()?;
	let environment = build_environment(&command.intent.environment)?;
	let creation = creation_policy(command);

	Ok(PreparedWindowsCommand {
		application_name,
		command_line,
		environment,
		current_dir,
		creation,
	})
}

fn validate_wrappers(wrappers: &[WrapperRegistration]) -> io::Result<()> {
	for wrapper in wrappers {
		if !supported_wrapper(wrapper.type_id) {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"Windows PTY spawning does not support command wrapper `{}`",
					wrapper.type_name
				),
			));
		}
	}
	Ok(())
}

fn supported_wrapper(_type_id: TypeId) -> bool {
	#[cfg(feature = "creation-flags")]
	if _type_id == TypeId::of::<CreationFlags>() {
		return true;
	}
	#[cfg(feature = "job-object")]
	if _type_id == TypeId::of::<JobObject>() {
		return true;
	}
	#[cfg(feature = "kill-on-drop")]
	if _type_id == TypeId::of::<KillOnDrop>() {
		return true;
	}
	false
}

fn creation_policy(_command: &PtyCommand) -> WindowsCreationPolicy {
	#[cfg(feature = "creation-flags")]
	let user_flags = _command
		.command
		.get_wrap::<CreationFlags>()
		.map_or(PROCESS_CREATION_FLAGS(0), |flags| flags.0);
	#[cfg(not(feature = "creation-flags"))]
	let user_flags = PROCESS_CREATION_FLAGS(0);

	#[cfg(feature = "job-object")]
	let has_job_object = _command.command.has_wrap::<JobObject>();
	#[cfg(not(feature = "job-object"))]
	let has_job_object = false;

	#[cfg(feature = "job-object")]
	let (spawn_flags, resume_after_assignment) = if has_job_object {
		let policy = job_creation_flags(user_flags);
		(policy.flags, policy.resume_after_assignment)
	} else {
		(user_flags, false)
	};
	#[cfg(not(feature = "job-object"))]
	let (spawn_flags, resume_after_assignment) = (user_flags, false);

	#[cfg(feature = "kill-on-drop")]
	let kill_on_drop = _command.command.has_wrap::<KillOnDrop>();
	#[cfg(not(feature = "kill-on-drop"))]
	let kill_on_drop = false;

	WindowsCreationPolicy {
		user_flags,
		spawn_flags,
		explicit_suspension: user_flags.contains(CREATE_SUSPENDED),
		has_job_object,
		resume_after_assignment,
		kill_on_drop,
	}
}

#[cfg(test)]
mod tests {
	use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

	#[cfg(all(feature = "creation-flags", feature = "job-object"))]
	use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

	use super::*;
	use crate::tokio::CommandWrapper;

	#[derive(Debug)]
	struct UnsupportedWrapper;

	impl CommandWrapper for UnsupportedWrapper {}

	fn prepare_without_parent(command: &PtyCommand) -> io::Result<PreparedWindowsCommand> {
		prepare_with(command, |_| Ok(PreparedEnvironment::Inherit))
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
	fn prepares_ordered_public_builder_intent() {
		let mut command = PtyCommand::new("tool");
		command
			.arg("two words")
			.raw_arg(r#"/D "literal""#)
			.args(["tail"])
			.envs([("Name", "child"), ("Second", "two")])
			.env_remove("gone");

		let prepared = prepare_with(&command, |intent| {
			environment::prepare_environment_with(intent, || {
				Ok(vec![
					environment::EnvironmentVariable {
						key: "Name".encode_utf16().collect(),
						value: "parent".encode_utf16().collect(),
					},
					environment::EnvironmentVariable {
						key: "gone".encode_utf16().collect(),
						value: "remove".encode_utf16().collect(),
					},
				])
			})
		})
		.unwrap();

		assert_eq!(
			prepared.command_line.as_units(),
			terminated(r#""tool" "two words" /D "literal" tail"#)
		);
		assert_eq!(
			prepared.environment,
			environment_block(&[("Name", "child"), ("Second", "two")])
		);
	}

	#[test]
	fn rejects_unknown_wrappers_before_preparing_the_environment() {
		let mut command = PtyCommand::new("tool");
		command.wrap(UnsupportedWrapper);

		let error = prepare_with(&command, |_| {
			panic!("unsupported wrappers must be rejected before environment preparation")
		})
		.unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
		assert!(error.to_string().contains("UnsupportedWrapper"), "{error}");
	}

	#[test]
	fn preserves_wtf16_current_directories() {
		let directory = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
		let mut command = PtyCommand::new("tool");
		command.current_dir(PathBuf::from(directory));

		let prepared = prepare_without_parent(&command).unwrap();
		assert_eq!(
			prepared.current_dir.unwrap().as_units(),
			[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800, 0]
		);
	}

	#[test]
	fn rejects_current_directory_nuls_before_preparing_the_environment() {
		let directory = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0]);
		let mut command = PtyCommand::new("tool");
		command.current_dir(PathBuf::from(directory));

		let error = prepare_with(&command, |_| {
			panic!("invalid cwd must be rejected before environment preparation")
		})
		.unwrap_err();
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
			let mut command = PtyCommand::new("tool");
			if reverse {
				command.wrap(JobObject).wrap(CreationFlags(user_flags));
			} else {
				command.wrap(CreationFlags(user_flags)).wrap(JobObject);
			}

			let policy = prepare_without_parent(&command).unwrap().creation;
			assert_eq!(policy.user_flags, user_flags);
			assert_eq!(policy.spawn_flags, user_flags | CREATE_SUSPENDED);
			assert!(!policy.explicit_suspension);
			assert!(policy.has_job_object);
			assert!(policy.resume_after_assignment);
			assert!(!policy.kill_on_drop);
		}
	}

	#[cfg(all(feature = "creation-flags", feature = "job-object"))]
	#[test]
	fn preserves_explicit_suspension_for_job_assignment() {
		let user_flags = CREATE_NO_WINDOW | CREATE_SUSPENDED;
		let mut command = PtyCommand::new("tool");
		command.wrap(CreationFlags(user_flags)).wrap(JobObject);

		let policy = prepare_without_parent(&command).unwrap().creation;
		assert_eq!(policy.user_flags, user_flags);
		assert_eq!(policy.spawn_flags, user_flags);
		assert!(policy.explicit_suspension);
		assert!(policy.has_job_object);
		assert!(!policy.resume_after_assignment);
	}

	#[cfg(feature = "creation-flags")]
	#[test]
	fn preserves_creation_flags_without_a_job_object() {
		use windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

		let mut command = PtyCommand::new("tool");
		command.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP));

		let policy = prepare_without_parent(&command).unwrap().creation;
		assert_eq!(policy.user_flags, CREATE_NEW_PROCESS_GROUP);
		assert_eq!(policy.spawn_flags, CREATE_NEW_PROCESS_GROUP);
		assert!(!policy.explicit_suspension);
		assert!(!policy.has_job_object);
		assert!(!policy.resume_after_assignment);
	}

	#[cfg(feature = "job-object")]
	#[test]
	fn derives_temporary_suspension_for_a_job_without_creation_flags() {
		let mut command = PtyCommand::new("tool");
		command.wrap(JobObject);

		let policy = prepare_without_parent(&command).unwrap().creation;
		assert_eq!(policy.user_flags, PROCESS_CREATION_FLAGS(0));
		assert_eq!(policy.spawn_flags, CREATE_SUSPENDED);
		assert!(!policy.explicit_suspension);
		assert!(policy.has_job_object);
		assert!(policy.resume_after_assignment);
	}

	#[cfg(feature = "kill-on-drop")]
	#[test]
	fn records_kill_on_drop_without_a_job_object() {
		let mut command = PtyCommand::new("tool");
		command.wrap(KillOnDrop);

		let policy = prepare_without_parent(&command).unwrap().creation;
		assert!(!policy.has_job_object);
		assert!(policy.kill_on_drop);
	}

	#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
	#[test]
	fn records_kill_on_drop_for_job_policy() {
		let mut command = PtyCommand::new("tool");
		command.wrap(JobObject).wrap(KillOnDrop);

		let policy = prepare_without_parent(&command).unwrap().creation;
		assert!(policy.has_job_object);
		assert!(policy.kill_on_drop);
	}
}
