//! Resolution of tracked program intent to the explicit `CreateProcessW` application name.

use std::{
	env,
	ffi::{OsStr, OsString},
	io,
	os::windows::ffi::OsStringExt,
	path::{Component, Path, PathBuf},
};

use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};

use super::{
	super::{CommandIntent, EnvChange},
	command::{WideCString, encode},
	environment::windows_equal,
};

pub(super) fn resolve(intent: &CommandIntent) -> io::Result<WideCString> {
	let program = &intent.program;
	let encoded = encode(program, "PTY program")?;
	if has_suffix(&encoded, b".bat") || has_suffix(&encoded, b".cmd") {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			"Windows PTY spawning does not execute batch scripts directly",
		));
	}

	let path = Path::new(program);
	if program.is_empty() || path.file_name().is_none() {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			"PTY program path has no file name",
		));
	}

	if !is_file_name(path, program) {
		if has_suffix(&encoded, b".exe") {
			return WideCString::from_os(program, "PTY program");
		}
		let with_exe = append_exe(path);
		if with_exe.exists() {
			return WideCString::from_os(with_exe.as_os_str(), "PTY program");
		}
		return WideCString::from_os(program, "PTY program");
	}

	let has_extension = encoded.contains(&(b'.' as u16));
	let child_path = child_path(intent)?;
	if let Some(path) = child_path.as_deref() {
		if let Some(program) = search_path(path, program, has_extension) {
			return WideCString::from_os(program.as_os_str(), "PTY program");
		}
	}
	if let Ok(mut directory) = env::current_exe() {
		directory.pop();
		if let Some(program) = candidate(directory, program, has_extension) {
			return WideCString::from_os(program.as_os_str(), "PTY program");
		}
	}
	if let Ok(directory) = system_directory() {
		if let Some(program) = candidate(directory, program, has_extension) {
			return WideCString::from_os(program.as_os_str(), "PTY program");
		}
	}
	if let Ok(directory) = windows_directory() {
		if let Some(program) = candidate(directory, program, has_extension) {
			return WideCString::from_os(program.as_os_str(), "PTY program");
		}
	}
	if let Some(path) = env::var_os("PATH") {
		if let Some(program) = search_path(&path, program, has_extension) {
			return WideCString::from_os(program.as_os_str(), "PTY program");
		}
	}

	Err(io::Error::new(
		io::ErrorKind::NotFound,
		"PTY program not found",
	))
}

fn child_path(intent: &CommandIntent) -> io::Result<Option<OsString>> {
	let path_key = "PATH".encode_utf16().collect::<Vec<_>>();
	let mut child_path = None;
	for change in &intent.environment.changes {
		let key = match change {
			EnvChange::Set(key, _) | EnvChange::Remove(key) => key,
		};
		if windows_equal(&encode(key, "PTY environment key")?, &path_key) {
			child_path = match change {
				EnvChange::Set(_, value) => Some(value.clone()),
				EnvChange::Remove(_) => None,
			};
		}
	}
	Ok(child_path)
}

fn search_path(paths: &OsStr, program: &OsStr, has_extension: bool) -> Option<PathBuf> {
	env::split_paths(paths)
		.filter(|path| !path.as_os_str().is_empty())
		.find_map(|path| candidate(path, program, has_extension))
}

fn candidate(mut directory: PathBuf, program: &OsStr, has_extension: bool) -> Option<PathBuf> {
	directory.push(program);
	if !has_extension {
		directory = append_exe(&directory);
	}
	directory.exists().then_some(directory)
}

fn append_exe(path: &Path) -> PathBuf {
	let mut path = path.as_os_str().to_os_string();
	path.push(".exe");
	PathBuf::from(path)
}

fn is_file_name(path: &Path, program: &OsStr) -> bool {
	let mut components = path.components();
	matches!(components.next(), Some(Component::Normal(name)) if name == program)
		&& components.next().is_none()
}

fn has_suffix(value: &[u16], suffix: &[u8]) -> bool {
	value.len() >= suffix.len()
		&& value[value.len() - suffix.len()..]
			.iter()
			.zip(suffix)
			.all(|(&left, &right)| {
				u8::try_from(left).is_ok_and(|left| left.eq_ignore_ascii_case(&right))
			})
}

fn system_directory() -> io::Result<PathBuf> {
	query_directory(|buffer| {
		// SAFETY: the optional slice is writable for its reported length.
		unsafe { GetSystemDirectoryW(buffer) }
	})
}

fn windows_directory() -> io::Result<PathBuf> {
	query_directory(|buffer| {
		// SAFETY: the optional slice is writable for its reported length.
		unsafe { GetWindowsDirectoryW(buffer) }
	})
}

fn query_directory(mut query: impl FnMut(Option<&mut [u16]>) -> u32) -> io::Result<PathBuf> {
	let required = query(None);
	if required == 0 {
		return Err(io::Error::last_os_error());
	}
	let mut buffer = vec![0; required as usize];
	loop {
		let length = query(Some(&mut buffer));
		if length == 0 {
			return Err(io::Error::last_os_error());
		}
		if (length as usize) < buffer.len() {
			buffer.truncate(length as usize);
			return Ok(PathBuf::from(OsString::from_wide(&buffer)));
		}
		buffer.resize(length as usize + 1, 0);
	}
}

#[cfg(test)]
mod tests {
	use std::{
		fs::File,
		os::windows::ffi::{OsStrExt, OsStringExt},
	};

	use tempfile::tempdir;

	use super::*;
	use crate::tokio::pty::EnvironmentIntent;

	fn intent(program: impl Into<OsString>) -> CommandIntent {
		CommandIntent {
			program: program.into(),
			args: Vec::new(),
			environment: EnvironmentIntent {
				clear: false,
				changes: Vec::new(),
			},
			current_dir: None,
		}
	}

	fn units(path: &OsStr) -> Vec<u16> {
		path.encode_wide().chain([0]).collect()
	}

	#[test]
	fn preserves_explicit_executable_paths_and_wtf16() {
		let current = env::current_exe().unwrap();
		let resolved = resolve(&intent(current.as_os_str())).unwrap();
		assert_eq!(resolved.as_units(), units(current.as_os_str()));

		let raw = OsString::from_wide(&[
			b'C' as u16,
			b':' as u16,
			b'\\' as u16,
			0xd800,
			b'.' as u16,
			b'e' as u16,
			b'x' as u16,
			b'e' as u16,
		]);
		let resolved = resolve(&intent(raw.clone())).unwrap();
		assert_eq!(resolved.as_units(), units(&raw));
	}

	#[test]
	fn searches_the_application_and_system_directories() {
		let current = env::current_exe().unwrap();
		let file_name = current.file_name().unwrap().to_os_string();
		let resolved = resolve(&intent(file_name)).unwrap();
		assert_eq!(resolved.as_units(), units(current.as_os_str()));

		let command = resolve(&intent("cmd")).unwrap();
		let command = PathBuf::from(OsString::from_wide(
			&command.as_units()[..command.as_units().len() - 1],
		));
		assert!(command.exists());
		assert!(
			command
				.file_name()
				.unwrap()
				.to_string_lossy()
				.eq_ignore_ascii_case("cmd.exe")
		);
	}

	#[test]
	fn searches_the_explicit_child_path_case_insensitively() {
		let directory = tempdir().unwrap();
		let executable = directory.path().join("tracked.exe");
		File::create(&executable).unwrap();
		let mut intent = intent("tracked");
		intent.environment.changes.push(EnvChange::Set(
			OsString::from("Path"),
			directory.path().as_os_str().to_os_string(),
		));

		let resolved = resolve(&intent).unwrap();
		assert_eq!(resolved.as_units(), units(executable.as_os_str()));
	}

	#[test]
	fn appends_exe_to_an_existing_direct_path() {
		let directory = tempdir().unwrap();
		let requested = directory.path().join("direct");
		let executable = directory.path().join("direct.exe");
		File::create(&executable).unwrap();

		let resolved = resolve(&intent(requested.as_os_str())).unwrap();
		assert_eq!(resolved.as_units(), units(executable.as_os_str()));
	}

	#[test]
	fn rejects_batch_files_and_missing_bare_programs() {
		for program in ["script.bat", "SCRIPT.CMD"] {
			let error = resolve(&intent(program)).unwrap_err();
			assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
			assert_eq!(
				error.to_string(),
				"Windows PTY spawning does not execute batch scripts directly"
			);
		}

		let error = resolve(&intent("process-wrap-definitely-missing-program")).unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::NotFound);
		assert_eq!(error.to_string(), "PTY program not found");
	}
}
