use std::{
	any::Any,
	io::{Read, Result},
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output},
};

#[cfg(unix)]
use nix::{
	sys::signal::{kill, Signal},
	unistd::Pid,
};

crate::generic_wrap::Wrap!(
	StdCommandWrap,
	Command,
	StdCommandWrapper,
	Child,
	StdChildWrapper,
	|child| child
);

/// Wrapper for `std::process::Child`.
///
/// This trait exposes most of the functionality of the underlying [`Child`]. It is implemented for
/// [`Child`] and by wrappers.
///
/// The required methods are `inner`, `inner_mut`, and `into_inner`. That provides access to the
/// lower layer and ultimately allows the wrappers to be unwrap and the `Child` to be used directly
/// if necessary. There are convenience `inner_child`, `inner_child_mut` and `into_inner_child`
/// methods on the trait object.
///
/// It also makes it possible for all the other methods to have default implementations. Some are
/// direct passthroughs to the lower layers, while others are more complex.
///
/// Here's a simple example of a wrapper:
///
/// ```rust
/// use process_wrap::std::*;
/// use std::process::Child;
///
/// #[derive(Debug)]
/// pub struct YourChildWrapper(Child);
///
/// impl StdChildWrapper for YourChildWrapper {
///     fn inner(&self) -> &dyn StdChildWrapper {
///         &self.0
///     }
///
///     fn inner_mut(&mut self) -> &mut dyn StdChildWrapper {
///         &mut self.0
///     }
///
///     fn into_inner(self: Box<Self>) -> Box<dyn StdChildWrapper> {
///         Box::new((*self).0)
///     }
/// }
/// ```
pub trait StdChildWrapper: Any + std::fmt::Debug + Send {
	/// Obtain a reference to the wrapped child.
	fn inner(&self) -> &dyn StdChildWrapper;

	/// Obtain a mutable reference to the wrapped child.
	fn inner_mut(&mut self) -> &mut dyn StdChildWrapper;

	/// Consume the current wrapper and return the wrapped child.
	///
	/// Note that this may disrupt whatever the current wrapper was doing. However, wrappers must
	/// ensure that the wrapped child is in a consistent state when this is called or they are
	/// dropped, so that this is always safe.
	fn into_inner(self: Box<Self>) -> Box<dyn StdChildWrapper>;

	/// Obtain a clone if possible.
	///
	/// Some implementations may make it possible to clone the implementing structure, even though
	/// std's `Child` isn't `Clone`. In those cases, this method should be overridden.
	fn try_clone(&self) -> Option<Box<dyn StdChildWrapper>> {
		None
	}

	/// Obtain the `Child`'s stdin.
	///
	/// By default this is a passthrough to the wrapped child.
	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		self.inner_mut().stdin()
	}

	/// Obtain the `Child`'s stdout.
	///
	/// By default this is a passthrough to the wrapped child.
	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		self.inner_mut().stdout()
	}

	/// Obtain the `Child`'s stderr.
	///
	/// By default this is a passthrough to the wrapped child.
	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		self.inner_mut().stderr()
	}

	/// Obtain the `Child`'s process ID.
	///
	/// In general this should be the PID of the top-level spawned process that was spawned
	/// However, that may vary depending on what a wrapper does.
	fn id(&self) -> u32 {
		self.inner().id()
	}

	/// Kill the `Child` and wait for it to exit.
	///
	/// By default this calls `start_kill()` and then `wait()`, which is the same way it is done on
	/// the underlying `Child`, but that way implementing either or both of those methods will use
	/// them when calling `kill()`, instead of requiring a stub implementation.
	fn kill(&mut self) -> Result<()> {
		self.start_kill()?;
		self.wait()?;
		Ok(())
	}

	/// Kill the `Child` without waiting for it to exit.
	///
	/// By default this is:
	/// - on Unix, sending a `SIGKILL` signal to the process;
	/// - otherwise, a passthrough to the underlying `kill()` method.
	///
	/// The `start_kill()` method doesn't exist on std's `Child`, and was introduced by Tokio. This
	/// library uses it to provide a consistent API across both std and Tokio (and because it's a
	/// generally useful API).
	fn start_kill(&mut self) -> Result<()> {
		self.inner_mut().start_kill()
	}

	/// Check if the `Child` has exited without blocking, and if so, return its exit status.
	///
	/// Wrappers must ensure that repeatedly calling this (or other wait methods) after the child
	/// has exited will always return the same result.
	///
	/// By default this is a passthrough to the underlying `Child`.
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	/// Wait for the `Child` to exit and return its exit status.
	///
	/// Wrappers must ensure that repeatedly calling this (or other wait methods) after the child
	/// has exited will always return the same result.
	///
	/// By default this is a passthrough to the underlying `Child`.
	fn wait(&mut self) -> Result<ExitStatus> {
		self.inner_mut().wait()
	}

	/// Wait for the `Child` to exit and return its exit status and outputs.
	///
	/// Note that this method reads the child's stdout and stderr to completion into memory.
	///
	/// On Unix, this reads from stdout and stderr simultaneously. On other platforms, it reads from
	/// stdout first, then stderr (pull requests welcome to improve this).
	///
	/// By default this is a reimplementation of the std method, so that it can use the wrapper's
	/// `wait()` method instead of the underlying `Child`'s `wait()`.
	fn wait_with_output(mut self: Box<Self>) -> Result<Output>
	where
		Self: 'static,
	{
		drop(self.stdin().take());

		let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
		match (self.stdout().take(), self.stderr().take()) {
			(None, None) => {}
			(Some(mut out), None) => {
				let res = out.read_to_end(&mut stdout);
				res.unwrap();
			}
			(None, Some(mut err)) => {
				let res = err.read_to_end(&mut stderr);
				res.unwrap();
			}
			(Some(out), Some(err)) => {
				let res = read2(out, &mut stdout, err, &mut stderr);
				res.unwrap();
			}
		}

		let status = self.wait()?;
		Ok(Output {
			status,
			stdout,
			stderr,
		})
	}

	/// Send a signal to the `Child`.
	///
	/// This method is only available on Unix. It doesn't exist on std's `Child`, nor on Tokio's. It
	/// was introduced by command-group to abstract over the signal behaviour between process groups
	/// and unwrapped processes.
	#[cfg(unix)]
	fn signal(&self, sig: i32) -> Result<()> {
		self.inner().signal(sig)
	}
}

impl StdChildWrapper for Child {
	fn inner(&self) -> &dyn StdChildWrapper {
		self
	}
	fn inner_mut(&mut self) -> &mut dyn StdChildWrapper {
		self
	}
	fn into_inner(self: Box<Self>) -> Box<dyn StdChildWrapper> {
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
	fn id(&self) -> u32 {
		Child::id(self)
	}
	fn start_kill(&mut self) -> Result<()> {
		#[cfg(unix)]
		{
			self.signal(Signal::SIGKILL as _)
		}

		#[cfg(not(unix))]
		{
			Child::kill(self)
		}
	}
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		Child::try_wait(self)
	}
	fn wait(&mut self) -> Result<ExitStatus> {
		Child::wait(self)
	}
	#[cfg(unix)]
	fn signal(&self, sig: i32) -> Result<()> {
		kill(
			Pid::from_raw(i32::try_from(self.id()).map_err(std::io::Error::other)?),
			Signal::try_from(sig)?,
		)
		.map_err(std::io::Error::from)
	}
}

impl dyn StdChildWrapper {
	fn downcast_ref<T: 'static>(&self) -> Option<&T> {
		(self as &dyn Any).downcast_ref()
	}

	fn is_raw_child(&self) -> bool {
		self.downcast_ref::<Child>().is_some()
	}

	/// Obtain a reference to the underlying [`Child`].
	pub fn inner_child(&self) -> &Child {
		let mut inner = self;
		while !inner.is_raw_child() {
			inner = inner.inner();
		}

		// UNWRAP: we've just checked that it's Some with is_raw_child()
		inner.downcast_ref().unwrap()
	}

	/// Obtain a mutable reference to the underlying [`Child`].
	///
	/// Modifying the raw child may be unsound depending on the layering of wrappers.
	pub unsafe fn inner_child_mut(&mut self) -> &mut Child {
		let mut inner = self;
		while !inner.is_raw_child() {
			inner = inner.inner_mut();
		}

		// UNWRAP: we've just checked that with is_raw_child()
		(inner as &mut dyn Any).downcast_mut().unwrap()
	}

	/// Obtain the underlying [`Child`].
	///
	/// Unwrapping everything may be unsound depending on the state of the wrappers.
	pub unsafe fn into_inner_child(self: Box<Self>) -> Child {
		let mut inner = self;
		while !inner.is_raw_child() {
			inner = inner.into_inner();
		}

		// UNWRAP: we've just checked that with is_raw_child()
		*(inner as Box<dyn Any>).downcast().unwrap()
	}
}

#[cfg(unix)]
fn read2(
	mut out_r: ChildStdout,
	out_v: &mut Vec<u8>,
	mut err_r: ChildStderr,
	err_v: &mut Vec<u8>,
) -> Result<()> {
	use nix::{
		errno::Errno,
		libc,
		poll::{poll, PollFd, PollFlags, PollTimeout},
	};
	use std::{
		io::Error,
		os::fd::{AsRawFd, BorrowedFd},
	};

	let out_fd = out_r.as_raw_fd();
	let err_fd = err_r.as_raw_fd();
	// SAFETY: these are dropped at the same time as all other FDs here
	let out_bfd = unsafe { BorrowedFd::borrow_raw(out_fd) };
	let err_bfd = unsafe { BorrowedFd::borrow_raw(err_fd) };

	set_nonblocking(out_bfd, true)?;
	set_nonblocking(err_bfd, true)?;

	let mut fds = [
		PollFd::new(out_bfd, PollFlags::POLLIN),
		PollFd::new(err_bfd, PollFlags::POLLIN),
	];

	loop {
		poll(&mut fds, PollTimeout::NONE)?;

		if fds[0].revents().is_some() && read(&mut out_r, out_v)? {
			set_nonblocking(err_bfd, false)?;
			return err_r.read_to_end(err_v).map(drop);
		}
		if fds[1].revents().is_some() && read(&mut err_r, err_v)? {
			set_nonblocking(out_bfd, false)?;
			return out_r.read_to_end(out_v).map(drop);
		}
	}

	fn read(r: &mut impl Read, dst: &mut Vec<u8>) -> Result<bool> {
		match r.read_to_end(dst) {
			Ok(_) => Ok(true),
			Err(e) => {
				if e.raw_os_error() == Some(libc::EWOULDBLOCK)
					|| e.raw_os_error() == Some(libc::EAGAIN)
				{
					Ok(false)
				} else {
					Err(e)
				}
			}
		}
	}

	#[cfg(target_os = "linux")]
	fn set_nonblocking(fd: BorrowedFd, nonblocking: bool) -> Result<()> {
		let v = nonblocking as libc::c_int;
		let res = unsafe { libc::ioctl(fd.as_raw_fd(), libc::FIONBIO, &v) };

		Errno::result(res).map_err(Error::from).map(drop)
	}

	#[cfg(not(target_os = "linux"))]
	fn set_nonblocking(fd: BorrowedFd, nonblocking: bool) -> Result<()> {
		use nix::fcntl::{fcntl, FcntlArg, OFlag};

		let mut flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
		flags.set(OFlag::O_NONBLOCK, nonblocking);

		fcntl(fd, FcntlArg::F_SETFL(flags))
			.map_err(Error::from)
			.map(drop)
	}
}

// if you're reading this code and despairing, we'd love
// your contribution of a proper read2 for your platform!
#[cfg(not(unix))]
fn read2(
	mut out_r: ChildStdout,
	out_v: &mut Vec<u8>,
	mut err_r: ChildStderr,
	err_v: &mut Vec<u8>,
) -> Result<()> {
	out_r.read_to_end(out_v)?;
	err_r.read_to_end(err_v)?;
	Ok(())
}
