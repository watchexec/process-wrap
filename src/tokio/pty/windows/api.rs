//! Runtime resolution of the ConPTY API surface.
//!
//! These functions must not appear in the crate's static import table: loading process-wrap on an
//! older Windows release must continue to work, with PTY spawning reporting `Unsupported` instead.

use std::{ffi::CStr, io, sync::OnceLock};

use windows::{
	Win32::{
		Foundation::{FARPROC, HANDLE},
		System::{
			Console::{COORD, HPCON},
			LibraryLoader::{GetModuleHandleW, GetProcAddress},
		},
	},
	core::{HRESULT, PCSTR, w},
};

pub(super) type CreatePseudoConsole =
	unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
pub(super) type ResizePseudoConsole = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
pub(super) type ClosePseudoConsole = unsafe extern "system" fn(HPCON);

#[derive(Clone, Copy, Debug)]
pub(super) struct ConPtyApi {
	pub(super) create: CreatePseudoConsole,
	pub(super) resize: ResizePseudoConsole,
	pub(super) close: ClosePseudoConsole,
}

pub(super) fn get() -> io::Result<&'static ConPtyApi> {
	static API: OnceLock<Option<ConPtyApi>> = OnceLock::new();
	API.get_or_init(resolve).as_ref().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::Unsupported,
			"Windows pseudo-console APIs are unavailable",
		)
	})
}

fn resolve() -> Option<ConPtyApi> {
	// SAFETY: kernel32.dll is loaded for every Windows process. GetModuleHandleW only borrows its
	// module handle, and GetProcAddress returns addresses that remain valid for the process lifetime.
	let kernel32 = unsafe { GetModuleHandleW(w!("kernel32.dll")) }.ok()?;
	resolve_with(|name| unsafe { GetProcAddress(kernel32, PCSTR(name.as_ptr().cast())) })
}

fn resolve_with(mut lookup: impl FnMut(&CStr) -> FARPROC) -> Option<ConPtyApi> {
	let create = lookup(c"CreatePseudoConsole")?;
	let resize = lookup(c"ResizePseudoConsole")?;
	let close = lookup(c"ClosePseudoConsole")?;

	// SAFETY: each address was resolved under the matching exported function name. Win32 function
	// pointers share one representation, and these signatures are the documented ConPTY ABI.
	Some(unsafe {
		ConPtyApi {
			create: std::mem::transmute::<unsafe extern "system" fn() -> isize, CreatePseudoConsole>(
				create,
			),
			resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, ResizePseudoConsole>(
				resize,
			),
			close: std::mem::transmute::<unsafe extern "system" fn() -> isize, ClosePseudoConsole>(
				close,
			),
		}
	})
}
