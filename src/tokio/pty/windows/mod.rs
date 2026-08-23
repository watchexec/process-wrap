use std::{any::TypeId, io};

use windows::Win32::System::Threading::{CREATE_SUSPENDED, PROCESS_CREATION_FLAGS};

#[cfg(feature = "creation-flags")]
use super::super::CreationFlags;
#[cfg(feature = "job-object")]
use super::super::JobObject;
#[cfg(feature = "kill-on-drop")]
use super::super::KillOnDrop;
use super::{EnvironmentIntent, PtyCommand, WrapperRegistration};
#[cfg(feature = "job-object")]
use crate::windows::job_creation_flags;
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

fn prepare(command: &PtyCommand) -> io::Result<PreparedWindowsCommand> {
	prepare_with(command, prepare_environment)
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
