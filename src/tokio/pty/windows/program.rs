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
