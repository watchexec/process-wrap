use std::{
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
	StdChild // |child| StdChild(child)
);

pub trait StdChildWrapper: std::fmt::Debug + Send + Sync {
	fn inner(&self) -> &Child;
	fn inner_mut(&mut self) -> &mut Child;
	fn into_inner(self: Box<Self>) -> Child;

	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		&mut self.inner_mut().stdin
	}

	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		&mut self.inner_mut().stdout
	}

	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		&mut self.inner_mut().stderr
	}

	fn id(&self) -> u32 {
		self.inner().id()
	}

	fn kill(&mut self) -> Result<()> {
		eprintln!("kill");
		self.start_kill()?;
		self.wait()?;
		Ok(())
	}

	fn start_kill(&mut self) -> Result<()> {
		#[cfg(unix)]
		{
			self.signal(Signal::SIGKILL)
		}

		#[cfg(not(unix))]
		{
			self.inner_mut().kill()
		}
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	fn wait(&mut self) -> Result<ExitStatus> {
		eprintln!("wait");
		self.inner_mut().wait()
	}

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

	#[cfg(unix)]
	fn signal(&self, sig: Signal) -> Result<()> {
		kill(
			Pid::from_raw(i32::try_from(self.id()).map_err(std::io::Error::other)?),
			sig,
		)
		.map_err(std::io::Error::from)
	}
}

// can't impl directly on Child as it would cause loops
#[derive(Debug)]
pub struct StdChild(pub Child);

impl StdChildWrapper for StdChild {
	fn inner(&self) -> &Child {
		&self.0
	}
	fn inner_mut(&mut self) -> &mut Child {
		&mut self.0
	}
	fn into_inner(self: Box<Self>) -> Child {
		(*self).0
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
		poll::{poll, PollFd, PollFlags},
	};
	use std::{
		io::Error,
		os::fd::{AsRawFd, BorrowedFd, RawFd},
	};

	let out_fd = out_r.as_raw_fd();
	let err_fd = err_r.as_raw_fd();
	set_nonblocking(out_fd, true)?;
	set_nonblocking(err_fd, true)?;

	// SAFETY: these are dropped at the same time as all other FDs here
	let out_bfd = unsafe { BorrowedFd::borrow_raw(out_fd) };
	let err_bfd = unsafe { BorrowedFd::borrow_raw(err_fd) };

	let mut fds = [
		PollFd::new(&out_bfd, PollFlags::POLLIN),
		PollFd::new(&err_bfd, PollFlags::POLLIN),
	];

	loop {
		poll(&mut fds, -1)?;

		if fds[0].revents().is_some() && read(&mut out_r, out_v)? {
			set_nonblocking(err_fd, false)?;
			return err_r.read_to_end(err_v).map(drop);
		}
		if fds[1].revents().is_some() && read(&mut err_r, err_v)? {
			set_nonblocking(out_fd, false)?;
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
	fn set_nonblocking(fd: RawFd, nonblocking: bool) -> Result<()> {
		let v = nonblocking as libc::c_int;
		let res = unsafe { libc::ioctl(fd, libc::FIONBIO, &v) };

		Errno::result(res).map_err(Error::from).map(drop)
	}

	#[cfg(not(target_os = "linux"))]
	fn set_nonblocking(fd: RawFd, nonblocking: bool) -> Result<()> {
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
