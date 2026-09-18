use std::{
	io,
	mem::{MaybeUninit, size_of},
	os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

use nix::libc;

const DESCRIPTOR_COUNT: usize = 2;
const DESCRIPTOR_BYTES: usize = size_of::<[RawFd; DESCRIPTOR_COUNT]>();
// SAFETY: the requested payload length is the size of exactly two RawFd values.
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
	// SAFETY: sockets is writable for two descriptors. SOCK_CLOEXEC makes descriptor installation
	// atomic with respect to a concurrent fork and exec.
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

	// SAFETY: socketpair initialized two independently owned descriptors on success.
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
/// `socket` and every descriptor in `descriptors` must be live for the duration of this call. This
/// function is suitable for the restricted child side of a post-fork helper: it uses only stack data,
/// pointer operations, `sendmsg`, and the thread-local errno slot.
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
	// SAFETY: an all-zero msghdr represents no address, vectors, or ancillary data. The fields used by
	// sendmsg are initialized below.
	let mut message = unsafe { MaybeUninit::<libc::msghdr>::zeroed().assume_init() };
	message.msg_iov = std::ptr::from_mut(&mut io_vector);
	message.msg_iovlen = 1 as _;

	let mut control = ControlBuffer::zeroed();
	if let Some(descriptors) = descriptors {
		message.msg_control = control.0.as_mut_ptr().cast();
		message.msg_controllen = control.0.len() as _;

		// SAFETY: the control buffer is aligned for cmsghdr, has CMSG_SPACE for two descriptors, and the
		// message points to the complete buffer.
		let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
		if header.is_null() {
			return false;
		}
		// SAFETY: header points into the live, sufficiently sized control buffer.
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
		// SAFETY: message points to live response, vector, and optional control-buffer storage.
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
	// SAFETY: an all-zero msghdr represents no address, vectors, or ancillary data. All receive storage
	// fields are initialized below.
	let mut message = unsafe { MaybeUninit::<libc::msghdr>::zeroed().assume_init() };
	message.msg_iov = std::ptr::from_mut(&mut io_vector);
	message.msg_iovlen = 1 as _;
	message.msg_control = control.0.as_mut_ptr().cast();
	message.msg_controllen = control.0.len() as _;

	let received = loop {
		// SAFETY: message points to writable response, vector, and control-buffer storage. The flag makes
		// every received descriptor close-on-exec as part of descriptor installation.
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

		// SAFETY: message's control pointer and length still refer to the live receive buffer.
		let header = unsafe { libc::CMSG_FIRSTHDR(message) };
		if header.is_null() {
			return (rights, false);
		}

		// SAFETY: zero is a valid ancillary payload length.
		let header_bytes = unsafe { libc::CMSG_LEN(0) as usize };
		// OpenBSD defines msg_controllen as socklen_t while Linux uses usize.
		#[allow(clippy::unnecessary_cast)]
		let available = message.msg_controllen as usize;
		// SAFETY: CMSG_FIRSTHDR returned a header within the receive buffer.
		let length = unsafe { (*header).cmsg_len as usize };
		if length < header_bytes || length > available {
			return (rights, false);
		}

		// SAFETY: the complete cmsghdr lies within the receive buffer after the bounds check above.
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

		// SAFETY: cmsg_len covers count complete descriptors and the destination has room for both.
		unsafe {
			std::ptr::copy_nonoverlapping(
				libc::CMSG_DATA(header).cast::<RawFd>(),
				rights.descriptors.as_mut_ptr(),
				count,
			);
		}
		rights.count = count;
		// SAFETY: count is bounded to the two-descriptor control buffer above.
		let expected_length =
			unsafe { libc::CMSG_LEN((count * size_of::<RawFd>()) as libc::c_uint) as usize };
		// SAFETY: count is bounded to the two-descriptor control buffer above.
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
		// SAFETY: SCM_RIGHTS installed two distinct owned descriptors, and this guard has relinquished
		// responsibility for closing them.
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
			// SAFETY: each descriptor was installed by SCM_RIGHTS and remains owned by this guard.
			unsafe {
				libc::close(*descriptor);
			}
		}
	}
}

#[cfg(target_os = "openbsd")]
unsafe fn errno() -> libc::c_int {
	// SAFETY: __errno returns the calling thread's live errno slot.
	unsafe { *libc::__errno() }
}

#[cfg(all(test, target_os = "linux"))]
unsafe fn errno() -> libc::c_int {
	// SAFETY: __errno_location returns the calling thread's live errno slot.
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
