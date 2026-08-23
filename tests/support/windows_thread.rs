use std::io::{Error, Result};

use windows::{
	Win32::{
		Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE},
		System::{
			Diagnostics::ToolHelp::{
				CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
				Thread32Next,
			},
			Threading::{OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME},
		},
	},
	core::HRESULT,
};

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
	fn drop(&mut self) {
		unsafe { CloseHandle(self.0) }.ok();
	}
}

pub fn process_has_suspended_thread(pid: u32) -> Result<bool> {
	let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	let mut entry = THREADENTRY32 {
		dwSize: std::mem::size_of::<THREADENTRY32>()
			.try_into()
			.expect("THREADENTRY32 size must fit in a DWORD"),
		..Default::default()
	};
	unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(Error::other)?;

	let mut found = false;
	let mut suspended = false;
	loop {
		if entry.th32OwnerProcessID == pid {
			found = true;
			let thread = OwnedHandle(unsafe {
				OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
			}?);
			let previous_count = unsafe { SuspendThread(thread.0) };
			if previous_count == u32::MAX {
				return Err(Error::last_os_error());
			}
			let balanced_count = unsafe { ResumeThread(thread.0) };
			if balanced_count == u32::MAX {
				return Err(Error::last_os_error());
			}
			if balanced_count != previous_count + 1 {
				return Err(Error::other("thread suspension probe was not balanced"));
			}
			suspended |= previous_count > 0;
		}

		match unsafe { Thread32Next(snapshot.0, &mut entry) } {
			Ok(()) => {}
			Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => break,
			Err(error) => return Err(Error::other(error)),
		}
	}

	if found {
		Ok(suspended)
	} else {
		Err(Error::other("no thread belonging to the child was found"))
	}
}
