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
		// SAFETY: every construction below wraps only a successful snapshot or thread-open result.
		// This private wrapper is that result's sole owner, and its one `Drop` invocation closes it
		// exactly once.
		unsafe { CloseHandle(self.0) }.ok();
	}
}

#[derive(Debug)]
pub struct ProcessGuard(Option<HANDLE>);

// SAFETY: Windows process handles are process-wide, not thread-affine, and support waiting,
// termination, and close from any thread. `ProcessGuard` privately owns the successful `OpenProcess`
// result in its `Option`; moving it moves that sole close/terminate responsibility, exposes no
// shared mutable Rust state, and only consuming or mutable methods remove the handle, so transfer
// cannot race a second fixture-owned close.
unsafe impl Send for ProcessGuard {}

impl ProcessGuard {
	pub fn open(pid: u32) -> Result<Self> {
		// SAFETY: a successful `OpenProcess` returns a newly owned handle for this fixture PID;
		// storing it in this guard makes the guard its sole closer.
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(Error::other)?;
		Ok(Self(Some(handle)))
	}

	pub fn has_exited(&self) -> Result<bool> {
		// SAFETY: `self.handle()` is the live successful `OpenProcess` result retained by this
		// borrowed guard, which cannot be disarmed or dropped for this call's duration.
		let wait = unsafe { WaitForSingleObject(self.handle(), 0) };
		if wait == WAIT_FAILED {
			Err(Error::last_os_error())
		} else {
			Ok(wait == WAIT_OBJECT_0)
		}
	}

	pub fn disarm(mut self) -> Result<()> {
		if let Some(handle) = self.0.take() {
			// SAFETY: consuming `self` and taking its only successful `OpenProcess` result leave no
			// `Drop` path owning this handle, so this is its single close.
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
			// SAFETY: this is the guard's still-owned successful `OpenProcess` result; taking it
			// prevents a second close after the best-effort termination.
			unsafe { TerminateProcess(handle, 1) }.ok();
			// SAFETY: the same taken handle remains live until this sole close.
			unsafe { CloseHandle(handle) }.ok();
		}
	}
}

pub fn resume_process_threads(pid: u32) -> Result<()> {
	// SAFETY: success returns a snapshot handle owned immediately by this `OwnedHandle`; it stays
	// live through all enumeration calls below and its `Drop` closes it exactly once.
	let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	let mut entry = THREADENTRY32 {
		dwSize: std::mem::size_of::<THREADENTRY32>()
			.try_into()
			.expect("THREADENTRY32 is guaranteed to fit in a DWORD"),
		..Default::default()
	};
	// SAFETY: `snapshot` is live, and `entry` is aligned stack storage initialized by `Default`
	// with `dwSize` set to exactly `THREADENTRY32`'s size; its mutable pointer lasts for the call.
	unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(Error::other)?;

	let mut found = false;
	loop {
		if entry.th32OwnerProcessID == pid {
			found = true;
			// SAFETY: this thread ID comes from the initialized current snapshot entry whose owner
			// equals the fixture's target PID. A successful result is immediately the sole handle in
			// `OwnedHandle`, and the requested right permits the following suspend/resume call.
			let thread = OwnedHandle(unsafe {
				OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
			}?);
			// SAFETY: `thread` still solely owns the successful `OpenThread` result and requested
			// `THREAD_SUSPEND_RESUME`, so its handle is live and authorized for this operation.
			if unsafe { ResumeThread(thread.0) } == u32::MAX {
				return Err(Error::last_os_error());
			}
		}

		// SAFETY: the live snapshot remains owned by `snapshot`, and `entry` retains its aligned,
		// initialized `THREADENTRY32` storage and exact `dwSize` for this call.
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
	// SAFETY: success returns a snapshot handle owned immediately by this `OwnedHandle`; it stays
	// live through all enumeration calls below and its `Drop` closes it exactly once.
	let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }?);
	let mut entry = THREADENTRY32 {
		dwSize: std::mem::size_of::<THREADENTRY32>()
			.try_into()
			.expect("THREADENTRY32 is guaranteed to fit in a DWORD"),
		..Default::default()
	};
	// SAFETY: `snapshot` is live, and `entry` is aligned stack storage initialized by `Default`
	// with `dwSize` set to exactly `THREADENTRY32`'s size; its mutable pointer lasts for the call.
	unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(Error::other)?;

	let mut found = false;
	let mut suspended = false;
	loop {
		if entry.th32OwnerProcessID == pid {
			found = true;
			// SAFETY: this thread ID comes from the initialized current snapshot entry whose owner
			// equals the fixture's target PID. A successful result is immediately the sole handle in
			// `OwnedHandle`, and the requested right permits the following suspend/resume call.
			let thread = OwnedHandle(unsafe {
				OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
			}?);
			// SAFETY: `thread` solely owns the live successful `OpenThread` result with
			// `THREAD_SUSPEND_RESUME`, so it is authorized for this balanced probe.
			let previous_count = unsafe { SuspendThread(thread.0) };
			if previous_count == u32::MAX {
				return Err(Error::last_os_error());
			}
			// SAFETY: `thread` still solely owns the live handle with the requested resume access;
			// this call balances the immediately preceding successful suspension probe.
			let balanced_count = unsafe { ResumeThread(thread.0) };
			if balanced_count == u32::MAX {
				return Err(Error::last_os_error());
			}
			if balanced_count != previous_count + 1 {
				return Err(Error::other("thread suspension probe was not balanced"));
			}
			suspended |= previous_count > 0;
		}

		// SAFETY: the live snapshot remains owned by `snapshot`, and `entry` retains its aligned,
		// initialized `THREADENTRY32` storage and exact `dwSize` for this call.
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
