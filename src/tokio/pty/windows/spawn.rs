//! Exact `CreateProcessW` spawning for a prepared ConPTY command.

use std::{
	ffi::c_void,
	io,
	mem::size_of,
	os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};

use windows::{
	Win32::{
		Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WIN32_ERROR},
		System::Threading::{
			CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
			GetCurrentProcess, GetProcessIdOfThread, INFINITE, OpenProcess, PROCESS_INFORMATION,
			PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
			TerminateProcess, WaitForSingleObject,
		},
	},
	core::{PCWSTR, PWSTR},
};

use super::{
	PreparedWindowsCommand, attributes::AttributeList, child::ConPtyChild,
	environment::PreparedEnvironment,
};

pub(super) fn spawn(
	mut command: PreparedWindowsCommand,
	attributes: &AttributeList,
	input_server: OwnedHandle,
	output_server: OwnedHandle,
) -> io::Result<SpawnedChild> {
	let startup = startup_info(attributes);

	let mut flags = command.creation.spawn_flags | EXTENDED_STARTUPINFO_PRESENT;
	let environment = match &command.environment {
		PreparedEnvironment::Inherit => None,
		PreparedEnvironment::Block(block) => {
			flags |= CREATE_UNICODE_ENVIRONMENT;
			Some(block.as_ptr().cast::<c_void>())
		}
	};
	let current_dir = command
		.current_dir
		.as_ref()
		.map_or(PCWSTR::null(), |directory| PCWSTR(directory.as_ptr()));
	let mut information = PROCESS_INFORMATION::default();

	// SAFETY: every pointer references live, NUL-terminated or explicitly bounded storage for the
	// duration of the call. The command line is uniquely mutable, the startup attribute list owns its
	// aligned backing storage, no handles are inherited, and STARTF_USESTDHANDLES with three null slots
	// asks ConPTY to install its console handles. The process-information value is writable output
	// storage.
	let created = unsafe {
		CreateProcessW(
			PCWSTR(command.application_name.as_ptr()),
			Some(PWSTR(command.command_line.as_mut_ptr())),
			None,
			None,
			false,
			flags,
			environment,
			current_dir,
			&startup.StartupInfo,
			&mut information,
		)
	};
	// ConPTY retains these synchronous endpoints internally. Dropping the parent copies before any
	// validation or handle duplication lets both host-facing channels observe closure on every path.
	drop(input_server);
	drop(output_server);
	created.map_err(win32_io_error)?;

	let handles = SpawnedProcess::new(information)?;
	let cleanup = handles.cleanup()?;
	let child = handles.into_child(command.creation.kill_on_drop);
	Ok(SpawnedChild { child, cleanup })
}

fn startup_info(attributes: &AttributeList) -> STARTUPINFOEXW {
	let mut startup = STARTUPINFOEXW::default();
	startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
	startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
	startup.StartupInfo.hStdInput = HANDLE::default();
	startup.StartupInfo.hStdOutput = HANDLE::default();
	startup.StartupInfo.hStdError = HANDLE::default();
	startup.lpAttributeList = attributes.as_ptr();
	startup
}

pub(super) struct SpawnedChild {
	pub(super) child: ConPtyChild,
	pub(super) cleanup: SpawnCleanup,
}

pub(super) struct SpawnCleanup {
	process: Option<OwnedHandle>,
}

impl SpawnCleanup {
	pub(super) fn disarm(mut self) {
		self.process.take();
	}
}

impl Drop for SpawnCleanup {
	fn drop(&mut self) {
		if let Some(process) = &self.process {
			terminate_and_wait(process);
		}
	}
}

struct SpawnedProcess {
	process: Option<OwnedHandle>,
	primary_thread: Option<OwnedHandle>,
	pid: u32,
}

impl SpawnedProcess {
	fn new(information: PROCESS_INFORMATION) -> io::Result<Self> {
		if information.hProcess.is_invalid() || information.hThread.is_invalid() {
			cleanup_invalid_information(information);
			return Err(io::Error::other(
				"CreateProcessW returned invalid process information",
			));
		}

		// SAFETY: CreateProcessW returned two distinct owned handles, each transferred exactly once.
		let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) };
		// SAFETY: as above, ownership of the primary-thread handle is transferred exactly once.
		let primary_thread = unsafe { OwnedHandle::from_raw_handle(information.hThread.0) };
		Ok(Self {
			process: Some(process),
			primary_thread: Some(primary_thread),
			pid: information.dwProcessId,
		})
	}

	fn cleanup(&self) -> io::Result<SpawnCleanup> {
		let process = self
			.process
			.as_ref()
			.expect("a spawned process guard must own its process handle");
		Ok(SpawnCleanup {
			process: Some(duplicate(process)?),
		})
	}

	fn into_child(mut self, kill_on_drop: bool) -> ConPtyChild {
		let process = self
			.process
			.take()
			.expect("a spawned process guard must own its process handle");
		let primary_thread = self
			.primary_thread
			.take()
			.expect("a spawned process guard must own its primary-thread handle");
		ConPtyChild::new(process, primary_thread, self.pid, kill_on_drop)
	}
}

impl Drop for SpawnedProcess {
	fn drop(&mut self) {
		if let Some(process) = &self.process {
			terminate_and_wait(process);
		}
	}
}

fn cleanup_invalid_information(information: PROCESS_INFORMATION) {
	let thread = (!information.hThread.is_invalid()).then(|| {
		// SAFETY: a non-invalid CreateProcessW output handle is uniquely owned here.
		unsafe { OwnedHandle::from_raw_handle(information.hThread.0) }
	});
	let pid = thread.as_ref().map_or(0, |thread| {
		// SAFETY: thread owns the live primary-thread handle returned by CreateProcessW.
		unsafe { GetProcessIdOfThread(HANDLE(thread.as_raw_handle())) }
	});
	let pid = if pid != 0 && (information.dwProcessId == 0 || information.dwProcessId == pid) {
		pid
	} else {
		0
	};
	let process = if !information.hProcess.is_invalid() {
		// SAFETY: a non-invalid CreateProcessW output handle is uniquely owned here.
		Some(unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) })
	} else if pid != 0 {
		// SAFETY: pid identifies the process CreateProcessW reported creating. The recovered handle is
		// non-inheritable and is transferred exactly once if it can still be opened.
		unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }
			.ok()
			.map(|handle| unsafe { OwnedHandle::from_raw_handle(handle.0) })
	} else {
		None
	};
	if let Some(process) = process {
		terminate_and_wait(&process);
	}
}

fn duplicate(handle: &OwnedHandle) -> io::Result<OwnedHandle> {
	// SAFETY: the current-process pseudo-handle and source process handle are live, duplicate points
	// to writable storage, and ownership of the non-inheritable result is transferred exactly once.
	unsafe {
		let current = GetCurrentProcess();
		let mut duplicate = HANDLE::default();
		DuplicateHandle(
			current,
			HANDLE(handle.as_raw_handle()),
			current,
			&mut duplicate,
			0,
			false,
			DUPLICATE_SAME_ACCESS,
		)
		.map_err(win32_io_error)?;
		Ok(OwnedHandle::from_raw_handle(duplicate.0))
	}
}

fn terminate_and_wait(process: &OwnedHandle) {
	let process = HANDLE(process.as_raw_handle());
	// SAFETY: the guard owns the process handle and is cleaning up a process which cannot be returned
	// to the caller. Waiting after termination prevents a detached live process.
	let _ = unsafe { TerminateProcess(process, 1) };
	// SAFETY: the process handle owner remains live until after this wait.
	let _ = unsafe { WaitForSingleObject(process, INFINITE) };
}

fn win32_io_error(error: windows::core::Error) -> io::Error {
	WIN32_ERROR::from_error(&error)
		.and_then(|error| i32::try_from(error.0).ok())
		.map_or_else(|| io::Error::other(error), io::Error::from_raw_os_error)
}

#[cfg(test)]
mod tests {
	use windows::Win32::System::Console::HPCON;

	use super::*;

	#[test]
	fn requests_conpty_replacement_for_all_standard_handles() {
		let attributes = AttributeList::new(HPCON(42)).unwrap();
		let startup = startup_info(&attributes);
		assert_eq!(startup.StartupInfo.dwFlags, STARTF_USESTDHANDLES);
		assert_eq!(startup.StartupInfo.hStdInput, HANDLE::default());
		assert_eq!(startup.StartupInfo.hStdOutput, HANDLE::default());
		assert_eq!(startup.StartupInfo.hStdError, HANDLE::default());
	}
}
