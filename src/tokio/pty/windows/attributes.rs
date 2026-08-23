//! Aligned ownership for a pseudo-console process-thread attribute list.

use std::{ffi::c_void, io, mem::size_of};

use windows::Win32::System::{
	Console::HPCON,
	Threading::{
		DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
		LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
		UpdateProcThreadAttribute,
	},
};

#[derive(Debug)]
pub(super) struct AttributeList {
	list: LPPROC_THREAD_ATTRIBUTE_LIST,
	storage: Box<[usize]>,
}

impl AttributeList {
	pub(super) fn new(pseudo_console: HPCON) -> io::Result<Self> {
		let mut bytes = 0;
		// SAFETY: a null first call is the documented size query and bytes is writable storage.
		let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
		if bytes == 0 {
			return Err(io::Error::last_os_error());
		}

		let words = bytes.div_ceil(size_of::<usize>());
		let mut storage = vec![0usize; words].into_boxed_slice();
		let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
		// SAFETY: storage is pointer-aligned, has at least the queried byte size, and remains owned by
		// the returned guard until after DeleteProcThreadAttributeList runs.
		unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes) }
			.map_err(io::Error::other)?;

		// Microsoft documents passing the HPCON value itself as lpValue, rather than a pointer to a
		// separate HPCON variable.
		// SAFETY: list is initialized for one attribute and pseudo_console is a live handle whose owner
		// outlives this list's use by CreateProcessW.
		if let Err(error) = unsafe {
			UpdateProcThreadAttribute(
				list,
				0,
				PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
				Some(pseudo_console.0 as *const c_void),
				size_of::<HPCON>(),
				None,
				None,
			)
		} {
			// SAFETY: the list was initialized successfully and has not yet been deleted.
			unsafe { DeleteProcThreadAttributeList(list) };
			return Err(io::Error::other(error));
		}

		Ok(Self { list, storage })
	}

	pub(super) fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
		self.list
	}
}

impl Drop for AttributeList {
	fn drop(&mut self) {
		// SAFETY: this guard owns one initialized list and drops it before its aligned storage.
		unsafe { DeleteProcThreadAttributeList(self.list) };
	}
}
