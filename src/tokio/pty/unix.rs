#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(any(target_os = "android", target_os = "linux"))]
use std::path::Path;
use std::{
	io,
	os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
	panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
	pin::Pin,
	process::Stdio,
	sync::{Arc, Mutex, Weak},
	task::{Context, Poll, ready},
};

#[cfg(any(
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
use nix::pty::openpty;
#[cfg(any(target_os = "android", target_os = "linux"))]
use nix::pty::ptsname_r;
#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
use nix::{
	fcntl::open,
	pty::{PtyMaster, grantpt, posix_openpt, unlockpt},
	sys::stat::Mode,
};
use nix::{
	fcntl::{FcntlArg, FdFlag, OFlag, fcntl},
	libc,
	pty::Winsize,
};
#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
use std::os::fd::IntoRawFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, unix::AsyncFd};

use super::{ControllerSlot, ProviderProduct, PtyChild, PtyController, PtySize, SpawnAttempt};
use crate::SpawnTransaction;

type Master = Arc<AsyncFd<OwnedFd>>;

#[derive(Debug)]
pub(super) struct Input {
	master: Option<Master>,
}

impl AsyncWrite for Input {
	fn poll_write(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &[u8],
	) -> Poll<io::Result<usize>> {
		if buffer.is_empty() {
			return Poll::Ready(Ok(0));
		}

		let Some(master) = self.get_mut().master.as_ref() else {
			return Poll::Ready(Err(closed()));
		};

		loop {
			let mut ready = ready!(master.poll_write_ready(cx))?;
			match ready.try_io(|master| write(master.get_ref(), buffer)) {
				Ok(result) => return Poll::Ready(result),
				Err(_would_block) => continue,
			}
		}
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		self.get_mut().master.take();
		Poll::Ready(Ok(()))
	}
}

#[derive(Debug)]
pub(super) struct Output {
	master: Master,
}

impl AsyncRead for Output {
	fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		if buffer.remaining() == 0 {
			return Poll::Ready(Ok(()));
		}

		loop {
			let mut ready = ready!(self.master.poll_read_ready(cx))?;
			let result =
				ready.try_io(|master| read(master.get_ref(), buffer.initialize_unfilled()));
			match result {
				Ok(Ok(read)) => {
					buffer.advance(read);
					return Poll::Ready(Ok(()));
				}
				Ok(Err(error)) if is_eof(&error) => return Poll::Ready(Ok(())),
				Ok(Err(error)) => return Poll::Ready(Err(error)),
				Err(_would_block) => continue,
			}
		}
	}
}

#[derive(Clone, Debug)]
pub(super) struct Resize {
	master: Weak<AsyncFd<OwnedFd>>,
}

impl Resize {
	pub(super) fn resize(&self, size: PtySize) -> io::Result<()> {
		let master = self.master.upgrade().ok_or_else(closed)?;
		set_size(master.get_ref(), size)
	}
}

pub(super) fn check_available() -> io::Result<()> {
	Ok(())
}

pub(super) fn spawn(attempt: &mut SpawnAttempt, size: PtySize) -> io::Result<ProviderProduct> {
	let kill_on_drop = attempt.kills_on_drop();
	let reset_sigmask = attempt.resets_sigmask();
	let (master, slave) = open_pty(size)?;
	let slave_stdin = duplicate(&slave)?;
	let slave_stdout = duplicate(&slave)?;

	let master = Arc::new(AsyncFd::new(master)?);
	let input = Input {
		master: Some(Arc::clone(&master)),
	};
	let resize = Resize {
		master: Arc::downgrade(&master),
	};
	let output = Output { master };
	let controller = Arc::new(ControllerSlot::new(PtyController::new(
		input, output, resize,
	)));

	let mut command = attempt.take_native_for_provider_spawn();
	command.kill_on_drop(kill_on_drop);
	let spawned = catch_unwind(AssertUnwindSafe(|| {
		with_slave_stdio(
			&mut command,
			slave_stdin,
			slave_stdout,
			slave,
			|command| {
				// SAFETY: the callback only invokes async-signal-safe libc functions and reports the
				// operating system's error without accessing shared process state.
				unsafe {
					command.pre_exec(move || setup_child(reset_sigmask));
				}
				command.spawn()
			},
		)
	}));
	let child = match spawned {
		Ok(child) => child?,
		Err(payload) => resume_unwind(payload),
	};
	let pid = child
		.id()
		.expect("Tokio reports a process ID for a newly spawned child");
	let child = Arc::new(Mutex::new(child));
	let transaction = PtyTransaction::new(Arc::clone(&child), Arc::clone(&controller));
	let child = PtyChild::new(child, pid, controller);

	Ok(ProviderProduct::new(Box::new(child), Box::new(transaction)))
}

fn with_slave_stdio<T>(
	command: &mut tokio::process::Command,
	stdin: OwnedFd,
	stdout: OwnedFd,
	stderr: OwnedFd,
	operation: impl FnOnce(&mut tokio::process::Command) -> T,
) -> T {
	command
		.stdin(Stdio::from(stdin))
		.stdout(Stdio::from(stdout))
		.stderr(Stdio::from(stderr));
	let result = catch_unwind(AssertUnwindSafe(|| operation(command)));

	// Drop every parent-side slave descriptor before the provider returns and process-wrap runs
	// post-spawn or child-wrapping hooks. Stdio::null stores no open descriptor on this attempt.
	command
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null());

	match result {
		Ok(result) => result,
		Err(payload) => resume_unwind(payload),
	}
}

#[derive(Debug)]
struct PtyTransaction {
	child: Arc<Mutex<tokio::process::Child>>,
	controller: Arc<ControllerSlot>,
	armed: bool,
}

impl PtyTransaction {
	fn new(
		child: Arc<Mutex<tokio::process::Child>>,
		controller: Arc<ControllerSlot>,
	) -> Self {
		Self {
			child,
			controller,
			armed: true,
		}
	}

	fn rollback_inner(&mut self) -> io::Result<()> {
		if !std::mem::replace(&mut self.armed, false) {
			return Ok(());
		}
		self.controller.rollback();
		terminate_and_reap(&self.child)
	}
}

impl SpawnTransaction for PtyTransaction {
	fn commit(&mut self) -> io::Result<()> {
		if self.armed {
			self.controller.commit();
			self.armed = false;
		}
		Ok(())
	}

	fn rollback(&mut self) -> io::Result<()> {
		self.rollback_inner()
	}
}

impl Drop for PtyTransaction {
	fn drop(&mut self) {
		let _ = self.rollback_inner();
	}
}

fn terminate_and_reap(child: &Mutex<tokio::process::Child>) -> io::Result<()> {
	let mut child = child
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	if child.try_wait()?.is_some() {
		return Ok(());
	}

	let pid = child
		.id()
		.expect("an unreaped Tokio child retains its process ID");
	let pid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;

	// The PTY setup makes the direct child the leader of a fresh session and process group. While the
	// child remains unreaped, its PID anchors that group identity, so signalling the negative PID cannot
	// target a recycled group. This also cleans up descendants which are still present at rollback time.
	// SAFETY: `pid` is positive and belongs to the live, unreaped child locked above.
	let group_error = if unsafe { libc::kill(-pid, libc::SIGKILL) } == -1 {
		let error = io::Error::last_os_error();
		(error.raw_os_error() != Some(libc::ESRCH)).then_some(error)
	} else {
		None
	};

	// Kill the direct child independently in case process-group signalling was unavailable. Holding the
	// only operational child lock keeps wait/kill state synchronized with every PTY child capability.
	if let Err(error) = child.start_kill()
		&& error.kind() != io::ErrorKind::InvalidInput
	{
		return Err(error);
	}

	loop {
		if child.try_wait()?.is_some() {
			return match group_error {
				Some(error) => Err(error),
				None => Ok(()),
			};
		}
		std::thread::yield_now();
	}
}

#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
fn open_pty(size: PtySize) -> io::Result<(OwnedFd, OwnedFd)> {
	let master = posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY)?;
	set_close_on_exec(&master)?;
	set_nonblocking(&master)?;
	grantpt(&master)?;
	unlockpt(&master)?;
	let slave = open_slave(&master)?;
	// SAFETY: ownership moves from PtyMaster into exactly one OwnedFd.
	let master = unsafe { OwnedFd::from_raw_fd(master.into_raw_fd()) };
	set_size(&master, size)?;
	Ok((master, slave))
}

#[cfg(any(
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
fn open_pty(size: PtySize) -> io::Result<(OwnedFd, OwnedFd)> {
	let size = winsize(size);
	let pair = openpty(Some(&size), None)?;
	set_close_on_exec(&pair.master)?;
	set_close_on_exec(&pair.slave)?;
	set_nonblocking(&pair.master)?;
	Ok((pair.master, pair.slave))
}

fn set_close_on_exec(fd: &impl AsFd) -> io::Result<()> {
	fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
	Ok(())
}

fn set_nonblocking(fd: &impl AsFd) -> io::Result<()> {
	let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
	fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
	Ok(())
}

fn duplicate(fd: &OwnedFd) -> io::Result<OwnedFd> {
	let duplicated = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(0))?;
	// SAFETY: F_DUPFD_CLOEXEC returned a new owned descriptor on success.
	Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

#[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
fn slave_flags() -> OFlag {
	OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_CLOEXEC
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn open_slave(master: &PtyMaster) -> io::Result<OwnedFd> {
	let name = ptsname_r(master)?;
	open(Path::new(&name), slave_flags(), Mode::empty()).map_err(io::Error::from)
}

#[cfg(target_os = "macos")]
fn open_slave(master: &PtyMaster) -> io::Result<OwnedFd> {
	let mut name = [0_u8; 128];
	// SAFETY: name is the 128-byte output buffer encoded by Darwin's TIOCPTYGNAME request.
	if unsafe {
		libc::ioctl(
			master.as_raw_fd(),
			libc::TIOCPTYGNAME.into(),
			name.as_mut_ptr(),
		)
	} == -1
	{
		return Err(io::Error::last_os_error());
	}
	let name = CStr::from_bytes_until_nul(&name)
		.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
	open(name, slave_flags(), Mode::empty()).map_err(io::Error::from)
}

fn winsize(size: PtySize) -> Winsize {
	Winsize {
		ws_row: size.rows,
		ws_col: size.columns,
		ws_xpixel: size.pixel_width,
		ws_ypixel: size.pixel_height,
	}
}

fn set_size(master: &OwnedFd, size: PtySize) -> io::Result<()> {
	let size = winsize(size);
	// SAFETY: master is a live PTY descriptor and size points to a valid winsize for the duration of
	// the ioctl call.
	if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == -1 {
		return Err(io::Error::last_os_error());
	}
	Ok(())
}

fn setup_child(reset_sigmask: bool) -> io::Result<()> {
	if reset_sigmask {
		crate::unix::reset_sigmask()?;
	}
	// SAFETY: this function runs after fork and before exec. Each call is async-signal-safe and uses
	// only the already-installed standard input descriptor.
	unsafe {
		if libc::setsid() == -1 {
			return Err(io::Error::last_os_error());
		}
		if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
			return Err(io::Error::last_os_error());
		}
		if libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) == -1 {
			return Err(io::Error::last_os_error());
		}
	}
	Ok(())
}

fn read(fd: &OwnedFd, buffer: &mut [u8]) -> io::Result<usize> {
	// SAFETY: the buffer is writable for its full length and remains live for the call.
	let read = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
	if read == -1 {
		return Err(io::Error::last_os_error());
	}
	Ok(read as usize)
}

fn write(fd: &OwnedFd, buffer: &[u8]) -> io::Result<usize> {
	// SAFETY: the buffer is readable for its full length and remains live for the call.
	let written = unsafe { libc::write(fd.as_raw_fd(), buffer.as_ptr().cast(), buffer.len()) };
	if written == -1 {
		return Err(io::Error::last_os_error());
	}
	Ok(written as usize)
}

fn is_eof(error: &io::Error) -> bool {
	error.raw_os_error() == Some(libc::EIO)
}

fn closed() -> io::Error {
	io::Error::new(io::ErrorKind::BrokenPipe, "the PTY master is closed")
}

#[cfg(test)]
mod tests {
	use std::os::fd::RawFd;

	use nix::unistd::pipe;

	use super::*;

	fn pipe_pair() -> (OwnedFd, OwnedFd) {
		let (reader, writer) = pipe().unwrap();
		set_nonblocking(&reader).unwrap();
		(reader, writer)
	}

	fn assert_open(fd: RawFd) {
		// SAFETY: the caller retains ownership of a descriptor which must still be open here.
		assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
	}

	fn assert_closed(reader: &OwnedFd) {
		let mut byte = 0_u8;
		// SAFETY: reader is a live nonblocking pipe descriptor and byte is writable for one byte.
		let read =
			unsafe { libc::read(reader.as_raw_fd(), std::ptr::from_mut(&mut byte).cast(), 1) };
		if read != 0 {
			panic!(
				"pipe reader did not observe writer closure: {}",
				io::Error::last_os_error()
			);
		}
	}

	fn descriptors() -> (OwnedFd, OwnedFd, OwnedFd, [OwnedFd; 3], [RawFd; 3]) {
		let (stdin_reader, stdin) = pipe_pair();
		let (stdout_reader, stdout) = pipe_pair();
		let (stderr_reader, stderr) = pipe_pair();
		let raw = [stdin.as_raw_fd(), stdout.as_raw_fd(), stderr.as_raw_fd()];
		(
			stdin,
			stdout,
			stderr,
			[stdin_reader, stdout_reader, stderr_reader],
			raw,
		)
	}

	#[test]
	fn winsize_preserves_character_and_pixel_dimensions() {
		let native = winsize(PtySize {
			rows: 31,
			columns: 97,
			pixel_width: 640,
			pixel_height: 480,
		});
		assert_eq!(native.ws_row, 31);
		assert_eq!(native.ws_col, 97);
		assert_eq!(native.ws_xpixel, 640);
		assert_eq!(native.ws_ypixel, 480);
	}

	#[test]
	fn allocated_master_has_close_on_exec_and_nonblocking() {
		let (master, _slave) = open_pty(PtySize::default()).unwrap();
		let descriptor_flags = FdFlag::from_bits_truncate(
			fcntl(&master, FcntlArg::F_GETFD).expect("read descriptor flags"),
		);
		let status_flags = OFlag::from_bits_truncate(
			fcntl(&master, FcntlArg::F_GETFL).expect("read status flags"),
		);
		assert!(descriptor_flags.contains(FdFlag::FD_CLOEXEC));
		assert!(status_flags.contains(OFlag::O_NONBLOCK));
	}

	#[test]
	fn slave_stdio_is_dropped_when_the_spawn_operation_returns() {
		let mut command = tokio::process::Command::new("ignored");
		let (stdin, stdout, stderr, readers, raw) = descriptors();

		let result = with_slave_stdio(&mut command, stdin, stdout, stderr, |_| {
			raw.into_iter().for_each(assert_open);
			42
		});

		assert_eq!(result, 42);
		readers.iter().for_each(assert_closed);
	}

	#[test]
	fn slave_stdio_is_dropped_when_the_spawn_operation_panics() {
		let mut command = tokio::process::Command::new("ignored");
		let (stdin, stdout, stderr, readers, raw) = descriptors();

		let panic = catch_unwind(AssertUnwindSafe(|| {
			with_slave_stdio(&mut command, stdin, stdout, stderr, |_| {
				raw.into_iter().for_each(assert_open);
				panic!("spawn operation panic");
			});
		}));

		assert!(panic.is_err());
		readers.iter().for_each(assert_closed);
	}
}
