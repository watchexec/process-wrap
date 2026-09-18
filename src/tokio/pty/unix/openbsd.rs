use std::{
	io,
	mem::{MaybeUninit, size_of},
	os::fd::{AsRawFd, OwnedFd, RawFd},
};

use nix::libc;

use super::{PtySize, descriptor_pair, set_nonblocking, set_size, verify_close_on_exec};

const PTM_DEVICE: &[u8] = b"/dev/ptm\0";
const IOC_OUT: libc::c_ulong = 0x4000_0000;
const IOCPARM_MASK: usize = 0x1fff;

#[repr(C)]
struct PtmGet {
	controller: RawFd,
	slave: RawFd,
	controller_name: [libc::c_char; 16],
	slave_name: [libc::c_char; 16],
}

const PTMGET: libc::c_ulong = IOC_OUT
	| ((size_of::<PtmGet>() & IOCPARM_MASK) as libc::c_ulong) << 16
	| (b't' as libc::c_ulong) << 8
	| 1;
const _: () = assert!(size_of::<PtmGet>() == 40);

pub(super) fn open_pty(size: PtySize) -> io::Result<(OwnedFd, OwnedFd)> {
	let (parent_socket, helper_socket) = descriptor_pair::socket_pair()?;
	// SAFETY: fork has no Rust-side invariants beyond separating execution by its return value. The child
	// immediately enters a syscall-only helper and exits without touching shared runtime state.
	let helper = unsafe { libc::fork() };
	if helper == -1 {
		return Err(io::Error::last_os_error());
	}
	if helper == 0 {
		// SAFETY: this is the post-fork helper. It uses only stack state and descriptor syscalls before
		// terminating through _exit, and never returns into the Rust runtime.
		unsafe { allocate_and_send(parent_socket.as_raw_fd(), helper_socket.as_raw_fd()) }
	}

	drop(helper_socket);
	let response = descriptor_pair::receive_response(&parent_socket);
	let helper_result = reap_helper(helper);
	let [master, slave] = match response {
		Ok(descriptors) => {
			helper_result?;
			descriptors
		}
		Err(error) => {
			let _ = helper_result;
			return Err(error);
		}
	};

	// MSG_CMSG_CLOEXEC installed both descriptors atomically. Verify that invariant before making the
	// master nonblocking and configuring the terminal dimensions.
	verify_close_on_exec(&master)?;
	verify_close_on_exec(&slave)?;
	set_nonblocking(&master)?;
	set_size(&master, size)?;
	Ok((master, slave))
}

unsafe fn allocate_and_send(parent_socket: RawFd, helper_socket: RawFd) -> ! {
	// SAFETY: both descriptors were inherited across fork and the helper does not use the parent end.
	unsafe {
		libc::close(parent_socket);
	}

	// SAFETY: PTM_DEVICE is a static NUL-terminated path.
	let ptm = unsafe { libc::open(PTM_DEVICE.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC) };
	if ptm == -1 {
		// SAFETY: __errno returns the helper thread's live errno slot.
		let error = unsafe { *libc::__errno() };
		// SAFETY: helper_socket remains live and no descriptors accompany this error response.
		let sent = unsafe { descriptor_pair::send_response(helper_socket, error, None) };
		// SAFETY: the helper must not run Rust destructors or shared runtime teardown after fork.
		unsafe { libc::_exit(if sent { 0 } else { 1 }) }
	}

	let mut pair = MaybeUninit::<PtmGet>::zeroed();
	// SAFETY: ptm is an open /dev/ptm descriptor, PTMGET encodes the exact PtmGet layout, and the output
	// pointer is writable for that complete structure.
	let allocated = unsafe { libc::ioctl(ptm, PTMGET, pair.as_mut_ptr()) };
	// Capture errno before close can change it.
	let error = if allocated == -1 {
		// SAFETY: __errno returns the helper thread's live errno slot.
		unsafe { *libc::__errno() }
	} else {
		0
	};
	// SAFETY: ptm is the helper's live, independently owned descriptor.
	unsafe {
		libc::close(ptm);
	}
	if allocated == -1 {
		// SAFETY: helper_socket remains live and no descriptors accompany this error response.
		let sent = unsafe { descriptor_pair::send_response(helper_socket, error, None) };
		// SAFETY: the helper must not run Rust destructors or shared runtime teardown after fork.
		unsafe { libc::_exit(if sent { 0 } else { 1 }) }
	}

	// SAFETY: PTMGET succeeded and initialized the complete structure.
	let pair = unsafe { pair.assume_init() };
	if pair.controller < 0 || pair.slave < 0 || pair.controller == pair.slave {
		if pair.controller >= 0 {
			// SAFETY: a nonnegative controller came from successful PTMGET.
			unsafe {
				libc::close(pair.controller);
			}
		}
		if pair.slave >= 0 && pair.slave != pair.controller {
			// SAFETY: a distinct nonnegative slave came from successful PTMGET.
			unsafe {
				libc::close(pair.slave);
			}
		}
		// SAFETY: helper_socket remains live and no descriptors accompany this error response.
		let sent = unsafe { descriptor_pair::send_response(helper_socket, libc::EIO, None) };
		// SAFETY: the helper must not run Rust destructors or shared runtime teardown after fork.
		unsafe { libc::_exit(if sent { 0 } else { 1 }) }
	}

	// PTMGET installs both descriptors without close-on-exec. They exist only in this no-exec helper;
	// SCM_RIGHTS transfers copies which recvmsg installs atomically with close-on-exec in the parent.
	// SAFETY: helper_socket and both PTY descriptors remain live through this call.
	let sent = unsafe {
		descriptor_pair::send_response(helper_socket, 0, Some([pair.controller, pair.slave]))
	};
	// SAFETY: both descriptors are independently owned by the helper. The successful send retained its
	// own references in the socket message until the parent receives them.
	unsafe {
		libc::close(pair.controller);
		libc::close(pair.slave);
		libc::_exit(if sent { 0 } else { 1 })
	}
}

fn reap_helper(helper: libc::pid_t) -> io::Result<()> {
	let mut status = 0;
	loop {
		// SAFETY: helper is the positive PID returned by fork and status is writable for one wait status.
		let waited = unsafe { libc::waitpid(helper, &mut status, 0) };
		if waited == helper {
			if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
				return Ok(());
			}
			return Err(io::Error::other(
				"the OpenBSD PTY allocation helper exited unsuccessfully",
			));
		}
		if waited == -1 {
			let error = io::Error::last_os_error();
			if error.kind() == io::ErrorKind::Interrupted {
				continue;
			}
			// An application may ignore SIGCHLD or use a process-wide child reaper. In either case ECHILD
			// means the short-lived helper no longer needs to be reaped here.
			if error.raw_os_error() == Some(libc::ECHILD) {
				return Ok(());
			}
			return Err(error);
		}
		return Err(io::Error::other(
			"waitpid returned an unexpected OpenBSD PTY helper process",
		));
	}
}
