use std::{
	future::Future,
	io,
	pin::Pin,
	process::ExitStatus,
	task::{Context, Poll},
};

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
use std::sync::{
	Arc, Mutex,
	atomic::{AtomicBool, Ordering},
};

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
use super::ChildWrapper;
use super::{Command, CommandWrapper, ProviderProduct, SpawnAttempt, SpawnProvider};

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
mod unix;
#[cfg(not(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
)))]
mod unsupported;
#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
use unix as imp;
#[cfg(not(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
)))]
use unsupported as imp;

/// A pseudo-terminal spawn provider for Tokio commands.
///
/// Register this like any other process wrapper. Spawning still returns the ordinary boxed Tokio
/// child contract; call `take_pty_controller()` on that child to take its terminal I/O and resize
/// controller once.
///
/// ```rust,no_run
/// # use std::io;
/// use process_wrap::tokio::{Command, Pty};
/// # fn run() -> io::Result<()> {
/// let mut command = Command::with_new("sh", |command| {
///     command.args(["-c", "printf terminal"]);
/// });
/// command.wrap(Pty::default());
/// let mut child = command.spawn()?;
/// let controller = child
///     .take_pty_controller()
///     .expect("a successful PTY spawn installs one controller");
/// # drop(controller);
/// # Ok(())
/// # }
/// # fn main() {}
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Pty {
	size: PtySize,
}

impl Pty {
	/// Construct a PTY provider with the requested initial terminal size.
	pub fn new(size: PtySize) -> Self {
		Self { size }
	}

	/// Return the configured initial terminal size.
	pub fn size(&self) -> PtySize {
		self.size
	}
}

impl CommandWrapper for Pty {
	fn extend(&mut self, other: Self) {
		*self = other;
	}

	fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
		Some(self)
	}
}

impl SpawnProvider for Pty {
	fn check_available(&self) -> io::Result<()> {
		imp::check_available()
	}

	fn validate_command(&self, _command: &Command) -> io::Result<()> {
		self.size.validate()
	}

	fn validate_attempt(&self, attempt: &SpawnAttempt, _command: &Command) -> io::Result<()> {
		#[cfg(unix)]
		{
			match attempt.process_group_target() {
				Some(crate::ProcessGroupTarget::AttachTo(_)) => {
					return Err(io::Error::new(
						io::ErrorKind::InvalidInput,
						"ProcessGroup::attach_to cannot be used with a PTY",
					));
				}
				Some(crate::ProcessGroupTarget::Leader) if attempt.creates_process_session() => {
					return Err(io::Error::new(
						io::ErrorKind::InvalidInput,
						"ProcessGroup and ProcessSession cannot both be used with a PTY",
					));
				}
				None | Some(crate::ProcessGroupTarget::Leader) => {}
			}
		}
		#[cfg(not(unix))]
		let _ = attempt;
		Ok(())
	}

	fn spawn(&self, attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<ProviderProduct> {
		imp::spawn(attempt, self.size)
	}
}

/// The character and pixel dimensions of a pseudo-terminal.
///
/// Character dimensions must both be nonzero. Pixel dimensions may be zero when they are unknown or
/// not meaningful to the caller. Sizes are validated before spawning and by [`PtyResize::resize`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
	/// Character rows.
	pub rows: u16,
	/// Character columns.
	pub columns: u16,
	/// Pixel width, or zero when unspecified.
	pub pixel_width: u16,
	/// Pixel height, or zero when unspecified.
	pub pixel_height: u16,
}

impl PtySize {
	/// Construct a character-cell size with unspecified pixel dimensions.
	pub fn new(rows: u16, columns: u16) -> io::Result<Self> {
		let size = Self {
			rows,
			columns,
			pixel_width: 0,
			pixel_height: 0,
		};
		size.validate()?;
		Ok(size)
	}

	/// Set the optional pixel dimensions.
	pub fn with_pixels(mut self, pixel_width: u16, pixel_height: u16) -> Self {
		self.pixel_width = pixel_width;
		self.pixel_height = pixel_height;
		self
	}

	/// Validate that both character dimensions are nonzero.
	pub fn validate(self) -> io::Result<()> {
		if self.rows == 0 || self.columns == 0 {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"PTY rows and columns must both be nonzero",
			));
		}
		Ok(())
	}
}

impl Default for PtySize {
	fn default() -> Self {
		Self {
			rows: 24,
			columns: 80,
			pixel_width: 0,
			pixel_height: 0,
		}
	}
}

/// The asynchronous input side of a pseudo-terminal.
///
/// Input and output are strong owners of the same bidirectional PTY master. Shutting down or
/// dropping only this input object releases its owner but cannot close the master or produce child
/// EOF while [`PtyOutput`] still exists. There is no independent transport half-close; send the
/// terminal's VEOF control character when that is the desired terminal policy.
#[derive(Debug)]
pub struct PtyInput {
	inner: imp::Input,
}

impl AsyncWrite for PtyInput {
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &[u8],
	) -> Poll<io::Result<usize>> {
		Pin::new(&mut self.inner).poll_write(cx, buffer)
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_flush(cx)
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_shutdown(cx)
	}
}

/// The asynchronous merged output side of a pseudo-terminal.
///
/// This is a strong owner of the shared bidirectional PTY master. On most supported Unix systems,
/// output EOF means that every slave descriptor has closed and a descendant may retain the slave
/// after the direct child exits. On macOS, drain output concurrently with waiting: session-leader
/// exit drains queued output and then revokes the controlling terminal from its descendants.
#[derive(Debug)]
pub struct PtyOutput {
	inner: imp::Output,
}

impl AsyncRead for PtyOutput {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buffer)
	}
}

/// A cloneable weak handle for resizing a pseudo-terminal.
///
/// Resize handles do not keep the PTY master alive.
#[derive(Clone, Debug)]
pub struct PtyResize {
	inner: imp::Resize,
}

impl PtyResize {
	/// Change the terminal size.
	///
	/// Returns [`io::ErrorKind::BrokenPipe`] after both strong I/O owners have been dropped.
	pub fn resize(&self, size: PtySize) -> io::Result<()> {
		size.validate()?;
		self.inner.resize(size)
	}
}

/// The I/O and resize controls for a spawned pseudo-terminal.
///
/// [`PtyInput`] and [`PtyOutput`] each strongly own one shared bidirectional master. The terminal
/// hangs up only after both owners are gone; dropping either one alone is not a half-close. A
/// [`PtyResize`] is weak and never keeps the terminal alive. There is deliberately no clonable
/// force-close handle, parent-terminal raw mode, byte relay, key handling, VT parsing, scrollback,
/// or pager policy in this transport.
///
/// Process supervision and PTY draining are separate lifecycles. Waiting for the direct child does
/// not imply output EOF on most supported Unix systems, because descendants can retain slave
/// descriptors. On macOS, drive waiting and draining concurrently because session-leader exit waits
/// for queued output before revoking the controlling terminal.
#[derive(Debug)]
pub struct PtyController {
	input: PtyInput,
	output: PtyOutput,
	resize: PtyResize,
}

impl PtyController {
	#[allow(dead_code)]
	fn new(input: imp::Input, output: imp::Output, resize: imp::Resize) -> Self {
		Self {
			input: PtyInput { inner: input },
			output: PtyOutput { inner: output },
			resize: PtyResize { inner: resize },
		}
	}

	/// Borrow the input side.
	pub fn input(&mut self) -> &mut PtyInput {
		&mut self.input
	}

	/// Borrow the merged output side.
	pub fn output(&mut self) -> &mut PtyOutput {
		&mut self.output
	}

	/// Clone the weak resize handle.
	pub fn resizer(&self) -> PtyResize {
		self.resize.clone()
	}

	/// Split the controller into independently owned input, output, and resize handles.
	pub fn into_parts(self) -> (PtyInput, PtyOutput, PtyResize) {
		(self.input, self.output, self.resize)
	}
}

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
#[derive(Debug)]
pub(super) struct ControllerSlot {
	controller: Mutex<Option<PtyController>>,
	committed: AtomicBool,
}

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
impl ControllerSlot {
	pub(super) fn new(controller: PtyController) -> Self {
		Self {
			controller: Mutex::new(Some(controller)),
			committed: AtomicBool::new(false),
		}
	}

	pub(super) fn commit(&self) {
		self.committed.store(true, Ordering::Release);
	}

	pub(super) fn rollback(&self) {
		self.controller
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take();
	}

	fn take(&self) -> Option<PtyController> {
		if !self.committed.load(Ordering::Acquire) {
			return None;
		}
		self.controller
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
	}
}

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
#[derive(Debug)]
pub(super) struct PtyChild {
	child: Arc<Mutex<Child>>,
	controller: Arc<ControllerSlot>,
	stdin: Option<ChildStdin>,
	stdout: Option<ChildStdout>,
	stderr: Option<ChildStderr>,
}

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
impl PtyChild {
	pub(super) fn new(child: Arc<Mutex<Child>>, controller: Arc<ControllerSlot>) -> Self {
		Self {
			child,
			controller,
			stdin: None,
			stdout: None,
			stderr: None,
		}
	}

	fn lock_child(&self) -> std::sync::MutexGuard<'_, Child> {
		self.child
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
	}
}

#[cfg(any(
	target_os = "android",
	target_os = "dragonfly",
	target_os = "freebsd",
	target_os = "illumos",
	target_os = "linux",
	target_os = "macos",
	target_os = "netbsd",
	target_os = "openbsd",
	target_os = "solaris"
))]
impl ChildWrapper for PtyChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		&mut self.stdin
	}

	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		&mut self.stdout
	}

	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		&mut self.stderr
	}

	fn id(&self) -> Option<u32> {
		self.lock_child().id()
	}

	fn start_kill(&mut self) -> io::Result<()> {
		self.lock_child().start_kill()
	}

	fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
		self.lock_child().try_wait()
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
		let child = Arc::clone(&self.child);
		Box::pin(std::future::poll_fn(move |cx| {
			let mut child = child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			let mut wait = Box::pin(child.wait());
			wait.as_mut().poll(cx)
		}))
	}

	#[cfg(unix)]
	fn signal(&self, sig: i32) -> io::Result<()> {
		ChildWrapper::signal(&*self.lock_child(), sig)
	}

	fn take_pty_controller_layer(&mut self) -> Option<PtyController> {
		self.controller.take()
	}
}
