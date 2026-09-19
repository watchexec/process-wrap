use std::{
	io,
	mem::{MaybeUninit, size_of},
	os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

use nix::libc;

const DESCRIPTOR_COUNT: usize = 2;
const DESCRIPTOR_BYTES: usize = size_of::<[RawFd; DESCRIPTOR_COUNT]>();
// SAFETY: DESCRIPTOR_BYTES is the representable byte length of exactly two RawFd values, so this
// supported-target CMSG_SPACE invocation computes control storage for precisely that payload.
const CONTROL_BYTES: usize = unsafe { libc::CMSG_SPACE(DESCRIPTOR_BYTES as libc::c_uint) as usize };

#[repr(C, align(16))]
struct ControlBuffer([u8; CONTROL_BYTES]);

impl ControlBuffer {
	fn zeroed() -> Self {
		Self([0; CONTROL_BYTES])
	}
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Response {
	error: libc::c_int,
}

pub(super) fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
	let mut sockets = [-1; 2];
	// SAFETY: sockets is aligned writable storage for exactly two c_int descriptors and stays live
	// for this synchronous call. SOCK_CLOEXEC makes each successful descriptor installation atomic
	// with respect to a concurrent fork and exec.
	if unsafe {
		libc::socketpair(
			libc::AF_UNIX,
			libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
			0,
			sockets.as_mut_ptr(),
		)
	} == -1
	{
		return Err(io::Error::last_os_error());
	}

	// SAFETY: a successful socketpair initialized both slots with distinct owned descriptors; each
	// from_raw_fd consumes one slot exactly once and gives it one OwnedFd close responsibility.
	Ok(unsafe {
		(
			OwnedFd::from_raw_fd(sockets[0]),
			OwnedFd::from_raw_fd(sockets[1]),
		)
	})
}

/// Sends either an operating-system error or exactly two descriptors.
///
/// # Safety
///
/// `socket` must remain a live, open socket descriptor and must not be concurrently closed,
/// replaced, or reused for the complete call. Every descriptor in `descriptors` must likewise
/// remain open and not be concurrently closed. `sendmsg` only copies their rights; it does not
/// transfer the caller's close responsibility. This function is suitable for the restricted child
/// side of a post-fork helper: it uses only stack data, pointer operations, `sendmsg`, and the
/// calling thread's errno slot.
pub(super) unsafe fn send_response(
	socket: RawFd,
	error: libc::c_int,
	descriptors: Option<[RawFd; DESCRIPTOR_COUNT]>,
) -> bool {
	let mut response = Response { error };
	let mut io_vector = libc::iovec {
		iov_base: std::ptr::from_mut(&mut response).cast(),
		iov_len: size_of::<Response>(),
	};
	// SAFETY: all-zero is a valid msghdr with null address/control pointers and zero lengths. The
	// only nonzero sendmsg fields are initialized below before the synchronous call.
	let mut message = unsafe { MaybeUninit::<libc::msghdr>::zeroed().assume_init() };
	message.msg_iov = std::ptr::from_mut(&mut io_vector);
	message.msg_iovlen = 1 as _;

	let mut control = ControlBuffer::zeroed();
	if let Some(descriptors) = descriptors {
		message.msg_control = control.0.as_mut_ptr().cast();
		message.msg_controllen = control.0.len() as _;

		// SAFETY: msg_control points at control's live, 16-byte-aligned backing array, and
		// msg_controllen reports its full CONTROL_BYTES capacity. CMSG_SPACE for this exact payload
		// makes CMSG_FIRSTHDR's returned cmsghdr and CMSG_LEN payload range fit in that array.
		let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
		if header.is_null() {
			return false;
		}
		// SAFETY: the preceding CMSG_FIRSTHDR proof bounds header and the entire two-RawFd payload in
		// control. On supported targets, the SCM_RIGHTS CMSG_DATA ABI places that payload at a
		// RawFd-aligned address when its cmsghdr is in this 16-byte-aligned ControlBuffer; that
		// separate ABI property, not CMSG_LEN, validates the typed destination cast. descriptors is
		// a separate live two-element source array, so the nonoverlapping copy writes only the
		// bounded payload before sendmsg observes it.
		unsafe {
			(*header).cmsg_len = libc::CMSG_LEN(DESCRIPTOR_BYTES as libc::c_uint) as _;
			(*header).cmsg_level = libc::SOL_SOCKET;
			(*header).cmsg_type = libc::SCM_RIGHTS;
			std::ptr::copy_nonoverlapping(
				descriptors.as_ptr(),
				libc::CMSG_DATA(header).cast::<RawFd>(),
				DESCRIPTOR_COUNT,
			);
		}
	}

	loop {
		// SAFETY: this function's contract keeps socket live, open, and neither closed nor reused while
		// sendmsg runs. message, response, and io_vector remain live; response is an initialized
		// repr(C) Response, and io_vector's sole entry gives sendmsg its exact byte length. msg_control
		// is either null or points to control's complete backing array, which remains live for the
		// synchronous call.
		let sent = unsafe { libc::sendmsg(socket, &message, 0) };
		if sent == size_of::<Response>() as libc::ssize_t {
			return true;
		}
		if sent != -1 {
			return false;
		}
		// SAFETY: errno returns the calling thread's live errno value.
		if unsafe { errno() } != libc::EINTR {
			return false;
		}
	}
}

pub(super) fn receive_response(socket: &OwnedFd) -> io::Result<[OwnedFd; DESCRIPTOR_COUNT]> {
	let mut response = MaybeUninit::<Response>::uninit();
	let mut io_vector = libc::iovec {
		iov_base: response.as_mut_ptr().cast(),
		iov_len: size_of::<Response>(),
	};
	let mut control = ControlBuffer::zeroed();
	// SAFETY: all-zero is a valid msghdr with null address/control pointers and zero lengths. Its
	// iovec, control pointer, and lengths are initialized below before recvmsg.
	let mut message = unsafe { MaybeUninit::<libc::msghdr>::zeroed().assume_init() };
	message.msg_iov = std::ptr::from_mut(&mut io_vector);
	message.msg_iovlen = 1 as _;
	message.msg_control = control.0.as_mut_ptr().cast();
	message.msg_controllen = control.0.len() as _;

	let received = loop {
		// SAFETY: socket's OwnedFd remains alive for this synchronous call. message's one iovec points
		// to response's aligned, writable Response-sized storage; msg_control points to control's
		// complete live 16-byte-aligned backing array with its capacity in msg_controllen. recvmsg
		// writes no more than those advertised ranges, and MSG_CMSG_CLOEXEC makes each received
		// descriptor installation close-on-exec atomically.
		let received =
			unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
		if received != -1 {
			break received;
		}
		let error = io::Error::last_os_error();
		if error.kind() != io::ErrorKind::Interrupted {
			return Err(error);
		}
	};

	let (mut rights, control_is_valid) = ReceivedRights::decode(&message);
	if received != size_of::<Response>() as libc::ssize_t
		|| message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
		|| !control_is_valid
	{
		return Err(protocol_error(
			"the PTY allocation helper returned a malformed response",
		));
	}

	// SAFETY: recvmsg initialized the complete response because its exact size was returned above.
	let response = unsafe { response.assume_init() };
	if response.error != 0 {
		if !rights.is_empty() || response.error < 0 {
			return Err(protocol_error(
				"the PTY allocation helper returned an invalid error",
			));
		}
		return Err(io::Error::from_raw_os_error(response.error));
	}

	rights.take_pair()
}

#[derive(Debug)]
struct ReceivedRights {
	descriptors: [RawFd; DESCRIPTOR_COUNT],
	count: usize,
}

impl ReceivedRights {
	fn empty() -> Self {
		Self {
			descriptors: [-1; DESCRIPTOR_COUNT],
			count: 0,
		}
	}

	fn decode(message: &libc::msghdr) -> (Self, bool) {
		let mut rights = Self::empty();
		if message.msg_controllen == 0 {
			return (rights, true);
		}

		// SAFETY: receive_response is this private function's only caller. It constructs message directly
		// above with msg_control pointing to control's live backing array and passes that exact capacity
		// to recvmsg; the kernel returns a control length within that array before decode is called.
		// Thus CMSG_FIRSTHDR may inspect the first complete cmsghdr while control remains in scope.
		let header = unsafe { libc::CMSG_FIRSTHDR(message) };
		if header.is_null() {
			return (rights, false);
		}

		// SAFETY: zero is a representable c_uint ancillary payload length, so CMSG_LEN computes the
		// supported-target cmsghdr byte length without accessing the receive buffer.
		let header_bytes = unsafe { libc::CMSG_LEN(0) as usize };
		// OpenBSD defines msg_controllen as socklen_t while Linux uses usize.
		#[allow(clippy::unnecessary_cast)]
		let available = message.msg_controllen as usize;
		// SAFETY: CMSG_FIRSTHDR established that header is a complete cmsghdr within control's live
		// receive range, so reading its cmsg_len is in bounds and properly aligned.
		let length = unsafe { (*header).cmsg_len as usize };
		if length < header_bytes || length > available {
			return (rights, false);
		}

		// SAFETY: cmsg_len is at least the complete cmsghdr length and no greater than the live
		// control range, as checked above, so both header field reads are in bounds.
		let is_rights = unsafe {
			(*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS
		};
		if !is_rights {
			return (rights, false);
		}

		let descriptor_bytes = length - header_bytes;
		let count = descriptor_bytes / size_of::<RawFd>();
		if descriptor_bytes % size_of::<RawFd>() != 0 || count > DESCRIPTOR_COUNT {
			return (rights, false);
		}

		// SAFETY: cmsg_len's checked header/payload range contains count complete RawFd values. The
		// current receive_response caller supplies a 16-byte-aligned ControlBuffer, and the supported-
		// target SCM_RIGHTS CMSG_DATA ABI aligns that payload for RawFd; this separate ABI property
		// validates the typed source cast. count is at most two, rights.descriptors has room for two,
		// and the separate control and rights allocations cannot overlap.
		unsafe {
			std::ptr::copy_nonoverlapping(
				libc::CMSG_DATA(header).cast::<RawFd>(),
				rights.descriptors.as_mut_ptr(),
				count,
			);
		}
		rights.count = count;
		// SAFETY: count is at most two, so its RawFd byte length is representable as c_uint and this
		// supported-target ancillary-size computation accesses no receive storage.
		let expected_length =
			unsafe { libc::CMSG_LEN((count * size_of::<RawFd>()) as libc::c_uint) as usize };
		// SAFETY: count is at most two, so its RawFd byte length is representable as c_uint and this
		// supported-target ancillary-size computation accesses no receive storage.
		let expected_space =
			unsafe { libc::CMSG_SPACE((count * size_of::<RawFd>()) as libc::c_uint) as usize };
		let valid = length == expected_length
			&& available >= expected_length
			&& available <= expected_space
			&& rights.descriptors[..count]
				.iter()
				.all(|descriptor| *descriptor >= 0)
			&& (count != 2 || rights.descriptors[0] != rights.descriptors[1]);
		(rights, valid)
	}

	fn is_empty(&self) -> bool {
		self.count == 0
	}

	fn take_pair(&mut self) -> io::Result<[OwnedFd; DESCRIPTOR_COUNT]> {
		if self.count != DESCRIPTOR_COUNT {
			return Err(protocol_error(
				"the PTY allocation helper did not return two descriptors",
			));
		}
		let descriptors = self.descriptors;
		self.count = 0;
		// SAFETY: recvmsg installed two distinct, nonnegative descriptors after the protocol checks
		// above. Setting count to zero first relinquishes this guard's close responsibility, and each
		// from_raw_fd then installs exactly one received descriptor in its corresponding OwnedFd.
		Ok(unsafe {
			[
				OwnedFd::from_raw_fd(descriptors[0]),
				OwnedFd::from_raw_fd(descriptors[1]),
			]
		})
	}
}

impl Drop for ReceivedRights {
	fn drop(&mut self) {
		for descriptor in &self.descriptors[..self.count] {
			// SAFETY: every indexed descriptor was copied from SCM_RIGHTS installation and has not been
			// transferred to OwnedFd because count still includes it. close consumes only this guard's
			// descriptor-table reference and this loop uses each stored descriptor once.
			unsafe {
				libc::close(*descriptor);
			}
		}
	}
}

#[cfg(target_os = "openbsd")]
unsafe fn errno() -> libc::c_int {
	// SAFETY: OpenBSD's __errno returns a non-null pointer to this calling thread's live errno
	// slot, which is read immediately before another operation can overwrite it.
	unsafe { *libc::__errno() }
}

#[cfg(all(test, target_os = "linux"))]
unsafe fn errno() -> libc::c_int {
	// SAFETY: __errno_location returns a non-null pointer to this calling thread's live errno
	// slot, which is read immediately before another operation can overwrite it.
	unsafe { *libc::__errno_location() }
}

fn protocol_error(message: &'static str) -> io::Error {
	io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
	use std::{
		fs::File,
		io::{Read, Write},
		os::{fd::AsRawFd, unix::net::UnixStream},
	};

	use nix::fcntl::{FcntlArg, FdFlag, fcntl};

	use super::*;

	#[test]
	fn received_descriptors_are_installed_close_on_exec() {
		let (sender, receiver) = socket_pair().unwrap();
		let (first, second) = UnixStream::pair().unwrap();
		// SAFETY: the socket and both transferred descriptors remain live through the call.
		assert!(unsafe {
			send_response(
				sender.as_raw_fd(),
				0,
				Some([first.as_raw_fd(), second.as_raw_fd()]),
			)
		});
		let [first_received, second_received] = receive_response(&receiver).unwrap();
		for descriptor in [&first_received, &second_received] {
			let flags = FdFlag::from_bits_truncate(fcntl(descriptor, FcntlArg::F_GETFD).unwrap());
			assert!(flags.contains(FdFlag::FD_CLOEXEC));
		}

		drop((first, second));
		let mut first_received = File::from(first_received);
		let mut second_received = File::from(second_received);
		first_received.write_all(b"x").unwrap();
		let mut byte = [0];
		second_received.read_exact(&mut byte).unwrap();
		assert_eq!(byte, *b"x");
	}

	#[test]
	fn helper_errors_are_returned_without_descriptors() {
		let (sender, receiver) = socket_pair().unwrap();
		// SAFETY: the socket remains live through the call and no descriptors are transferred.
		assert!(unsafe { send_response(sender.as_raw_fd(), libc::ENXIO, None) });
		assert_eq!(
			receive_response(&receiver).unwrap_err().raw_os_error(),
			Some(libc::ENXIO)
		);
	}
}
