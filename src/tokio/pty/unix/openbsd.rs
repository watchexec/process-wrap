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
	// SAFETY: the two OwnedFds remain live across this synchronous call. On a non-error parent
	// return, the caller receives a positive helper PID and continues normal Rust execution; the
	// child immediately passes non-owning raw descriptor views to the syscall-only branch below and
	// terminates via _exit.
	let helper = unsafe { libc::fork() };
	if helper == -1 {
		return Err(io::Error::last_os_error());
	}
	if helper == 0 {
		// SAFETY: fork selected this post-fork child; both raw descriptors are inherited, live, and
		// distinct. allocate_and_send uses only its documented syscall-oriented path and _exit,
		// so this branch cannot return into or unwind through the Rust runtime.
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

/// # Safety
///
/// `parent_socket` and `helper_socket` must be distinct, live socket descriptors inherited by the
/// immediate post-fork child. Call this function only in that child. Its body relies on OpenBSD's
/// libc implementations of the required descriptor, ancillary-message, and `PTMGET` ioctl
/// operations being direct syscall-oriented operations without allocation or locks in this path;
/// this is a supported-target reliance, not a portable POSIX async-signal-safety guarantee. It
/// must use only that syscall-oriented path, never return or unwind into Rust teardown, and
/// terminate every path through `_exit`.
unsafe fn allocate_and_send(parent_socket: RawFd, helper_socket: RawFd) -> ! {
	// SAFETY: parent_socket is this child's inherited descriptor-table reference to the parent
	// endpoint; it is live by the function contract, is never used again here, and close consumes
	// only that reference. The inherited OwnedFd backing this raw view is never dropped because
	// this helper's documented _exit-only path cannot return into Rust teardown.
	unsafe {
		libc::close(parent_socket);
	}

	// SAFETY: PTM_DEVICE is static NUL-terminated byte storage whose pointer remains valid for this
	// synchronous open call; its result is either -1 or one new raw descriptor owned by this helper.
	let ptm = unsafe { libc::open(PTM_DEVICE.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC) };
	if ptm == -1 {
		// SAFETY: OpenBSD's __errno returns a non-null pointer to this helper thread's live errno
		// slot, which is read before send_response or any other operation can overwrite it.
		let error = unsafe { *libc::__errno() };
		// SAFETY: helper_socket is this child's still-live inherited helper endpoint; None transfers no
		// descriptors, and send_response's stack-backed response/control storage remains live for its call.
		let sent = unsafe { descriptor_pair::send_response(helper_socket, error, None) };
		// SAFETY: _exit consumes only this scalar status after send_response returns; it prevents Rust
		// destructors, allocator use, unwinding, and shared-runtime teardown in the post-fork child.
		unsafe { libc::_exit(if sent { 0 } else { 1 }) }
	}

	let mut pair = MaybeUninit::<PtmGet>::zeroed();
	// SAFETY: ptm is this helper's open raw /dev/ptm descriptor. PTMGET encodes the exact repr(C)
	// PtmGet ABI layout, and pair provides aligned writable storage for that complete structure until
	// ioctl returns. POSIX does not mandate this ioctl as async-signal-safe; this post-fork call relies
	// on the supported OpenBSD libc implementation being direct syscall-oriented without allocation
	// or locks.
	let allocated = unsafe { libc::ioctl(ptm, PTMGET, pair.as_mut_ptr()) };
	// Capture errno before close can change it.
	let error = if allocated == -1 {
		// SAFETY: OpenBSD's __errno returns a non-null pointer to this helper thread's live errno
		// slot, which is read before send_response or any other operation can overwrite it.
		unsafe { *libc::__errno() }
	} else {
		0
	};
	// SAFETY: ptm is the helper's one live raw descriptor reference returned by open; errno was
	// captured first, and close consumes that reference before this value is never used again.
	unsafe {
		libc::close(ptm);
	}
	if allocated == -1 {
		// SAFETY: helper_socket is this child's still-live inherited helper endpoint; None transfers no
		// descriptors, and send_response's stack-backed response/control storage remains live for its call.
		let sent = unsafe { descriptor_pair::send_response(helper_socket, error, None) };
		// SAFETY: _exit consumes only this scalar status after send_response returns; it prevents Rust
		// destructors, allocator use, unwinding, and shared-runtime teardown in the post-fork child.
		unsafe { libc::_exit(if sent { 0 } else { 1 }) }
	}

	// SAFETY: a successful PTMGET initialized pair's complete repr(C) PtmGet storage before returning;
	// the value is now read only after ptm's unrelated descriptor-table reference was closed.
	let pair = unsafe { pair.assume_init() };
	if pair.controller < 0 || pair.slave < 0 || pair.controller == pair.slave {
		if pair.controller >= 0 {
			// SAFETY: successful PTMGET installed this nonnegative controller descriptor as a helper-owned
			// raw reference; this branch closes it exactly once and never uses it afterward.
			unsafe {
				libc::close(pair.controller);
			}
		}
		if pair.slave >= 0 && pair.slave != pair.controller {
			// SAFETY: successful PTMGET installed this distinct nonnegative slave descriptor as a
			// helper-owned raw reference; the preceding comparison prevents a duplicate close.
			unsafe {
				libc::close(pair.slave);
			}
		}
		// SAFETY: helper_socket is this child's still-live inherited helper endpoint; None transfers no
		// descriptors, and send_response's stack-backed response/control storage remains live for its call.
		let sent = unsafe { descriptor_pair::send_response(helper_socket, libc::EIO, None) };
		// SAFETY: _exit consumes only this scalar status after send_response returns; it prevents Rust
		// destructors, allocator use, unwinding, and shared-runtime teardown in the post-fork child.
		unsafe { libc::_exit(if sent { 0 } else { 1 }) }
	}

	// PTMGET installs both descriptors without close-on-exec. They exist only in this no-exec helper;
	// SCM_RIGHTS copies their rights, which recvmsg installs atomically with close-on-exec in the parent.
	// SAFETY: helper_socket and both distinct nonnegative PTY descriptors remain live through the
	// synchronous send_response call. Its stack-backed msghdr/control storage copies the two raw
	// values; it does not transfer this helper's close responsibility.
	let sent = unsafe {
		descriptor_pair::send_response(helper_socket, 0, Some([pair.controller, pair.slave]))
	};
	// SAFETY: PTMGET gave the helper distinct raw descriptor references. send_response has returned,
	// so its buffers no longer borrow their values; a successful send left independent socket-message
	// references for the parent, while these two local references are each closed exactly once. _exit
	// then prevents every Rust teardown path.
	unsafe {
		libc::close(pair.controller);
		libc::close(pair.slave);
		libc::_exit(if sent { 0 } else { 1 })
	}
}

fn reap_helper(helper: libc::pid_t) -> io::Result<()> {
	let mut status = 0;
	loop {
		// SAFETY: helper is the positive PID returned by this open_pty invocation's successful fork, and
		// status is aligned writable storage for exactly one wait status that remains live for waitpid.
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
