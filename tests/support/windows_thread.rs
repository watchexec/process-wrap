use std::io::{Error, Result};

use windows::{
	Win32::{
		Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
		System::{
			Diagnostics::ToolHelp::{
				CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
				Thread32Next,
			},
			Threading::{
				OpenProcess, OpenThread, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread,
				SuspendThread, THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
			},
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

#[derive(Debug)]
pub struct ProcessGuard(Option<HANDLE>);

// Windows process handles may be used and closed from any thread while this guard preserves unique
// ownership of the handle value.
unsafe impl Send for ProcessGuard {}

impl ProcessGuard {
	pub fn open(pid: u32) -> Result<Self> {
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(Error::other)?;
		Ok(Self(Some(handle)))
	}

	pub fn has_exited(&self) -> Result<bool> {
		let wait = unsafe { WaitForSingleObject(self.handle(), 0) };
		if wait == WAIT_FAILED {
			Err(Error::last_os_error())
		} else {
			Ok(wait == WAIT_OBJECT_0)
		}
	}

	pub fn disarm(mut self) -> Result<()> {
		if let Some(handle) = self.0.take() {
			unsafe { CloseHandle(handle) }?;
		}
		Ok(())
	}

	fn handle(&self) -> HANDLE {
		self.0
			.expect("only ProcessGuard::disarm clears the handle, and it consumes the guard")
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		if let Some(handle) = self.0.take() {
			unsafe { TerminateProcess(handle, 1) }.ok();
			unsafe { CloseHandle(handle) }.ok();
		}
	}
}

pub fn resume_process_threads(pid: u32) -> Result<()> {
	let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	let mut entry = THREADENTRY32 {
		dwSize: std::mem::size_of::<THREADENTRY32>()
			.try_into()
			.expect("THREADENTRY32 is guaranteed to fit in a DWORD"),
		..Default::default()
	};
	unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(Error::other)?;

	let mut found = false;
	loop {
		if entry.th32OwnerProcessID == pid {
			found = true;
			let thread = OwnedHandle(unsafe {
				OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
			}?);
			if unsafe { ResumeThread(thread.0) } == u32::MAX {
				return Err(Error::last_os_error());
			}
		}

		match unsafe { Thread32Next(snapshot.0, &mut entry) } {
			Ok(()) => {}
			Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => break,
			Err(error) => return Err(Error::other(error)),
		}
	}

	if found {
		Ok(())
	} else {
		Err(Error::other("no thread belonging to the child was found"))
	}
}

pub fn process_has_suspended_thread(pid: u32) -> Result<bool> {
	let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	let mut entry = THREADENTRY32 {
		dwSize: std::mem::size_of::<THREADENTRY32>()
			.try_into()
			.expect("THREADENTRY32 is guaranteed to fit in a DWORD"),
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
