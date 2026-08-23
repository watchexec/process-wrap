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

#[cfg(test)]
mod tests {
	use super::*;

	unsafe extern "system" fn create(
		_size: COORD,
		_input: HANDLE,
		_output: HANDLE,
		_flags: u32,
		_pseudo_console: *mut HPCON,
	) -> HRESULT {
		HRESULT(0)
	}

	unsafe extern "system" fn resize(_pseudo_console: HPCON, _size: COORD) -> HRESULT {
		HRESULT(0)
	}

	unsafe extern "system" fn close(_pseudo_console: HPCON) {}

	fn create_proc() -> FARPROC {
		// SAFETY: tests erase this pointer only so resolve_with can restore its original signature.
		Some(unsafe {
			std::mem::transmute::<CreatePseudoConsole, unsafe extern "system" fn() -> isize>(create)
		})
	}

	fn resize_proc() -> FARPROC {
		// SAFETY: tests erase this pointer only so resolve_with can restore its original signature.
		Some(unsafe {
			std::mem::transmute::<ResizePseudoConsole, unsafe extern "system" fn() -> isize>(resize)
		})
	}

	fn close_proc() -> FARPROC {
		// SAFETY: tests erase this pointer only so resolve_with can restore its original signature.
		Some(unsafe {
			std::mem::transmute::<ClosePseudoConsole, unsafe extern "system" fn() -> isize>(close)
		})
	}

	fn lookup(name: &CStr) -> FARPROC {
		match name.to_bytes() {
			b"CreatePseudoConsole" => create_proc(),
			b"ResizePseudoConsole" => resize_proc(),
			b"ClosePseudoConsole" => close_proc(),
			_ => None,
		}
	}

	#[test]
	fn resolves_all_three_exports_by_exact_name() {
		let mut requested = Vec::new();
		let api = resolve_with(|name| {
			requested.push(name.to_bytes().to_vec());
			lookup(name)
		})
		.unwrap();

		let expected_create: CreatePseudoConsole = create;
		let expected_resize: ResizePseudoConsole = resize;
		let expected_close: ClosePseudoConsole = close;
		assert_eq!(api.create as *const (), expected_create as *const ());
		assert_eq!(api.resize as *const (), expected_resize as *const ());
		assert_eq!(api.close as *const (), expected_close as *const ());
		assert_eq!(
			requested,
			[
				b"CreatePseudoConsole".to_vec(),
				b"ResizePseudoConsole".to_vec(),
				b"ClosePseudoConsole".to_vec(),
			]
		);
	}

	#[test]
	fn rejects_an_incomplete_capability_set() {
		for missing in [
			b"CreatePseudoConsole".as_slice(),
			b"ResizePseudoConsole".as_slice(),
			b"ClosePseudoConsole".as_slice(),
		] {
			let api = resolve_with(|name| {
				if name.to_bytes() == missing {
					None
				} else {
					lookup(name)
				}
			});
			assert!(
				api.is_none(),
				"missing {}",
				String::from_utf8_lossy(missing)
			);
		}
	}
}
