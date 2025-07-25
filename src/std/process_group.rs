use std::{
	io::{Error, Result},
	ops::ControlFlow,
	os::unix::process::{CommandExt, ExitStatusExt},
	process::{Command, ExitStatus},
};

use nix::{
	errno::Errno,
	libc,
	sys::{
		signal::{killpg, Signal},
		wait::WaitPidFlag,
	},
	unistd::Pid,
};
#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::ChildExitStatus;

use super::{ChildWrapper, CommandWrap, CommandWrapper};

/// Wrapper which sets the process group of a `Command`.
///
/// This wrapper is only available on Unix.
///
/// It sets the process group of a [`Command`], either to itself as the leader of a new group, or to
/// an existing one by its PGID. See [setpgid(2)](https://pubs.opengroup.org/onlinepubs/9699919799/functions/setpgid.html).
///
/// Process groups direct signals to all members of the group, and also serve to control job
/// placement in foreground or background, among other actions.
///
/// This wrapper provides a child wrapper: [`ProcessGroupChild`].
#[derive(Clone, Copy, Debug)]
pub struct ProcessGroup {
	leader: Pid,
}

impl ProcessGroup {
	/// Create a process group wrapper setting up a new process group with the command as the leader.
	pub fn leader() -> Self {
		Self {
			leader: Pid::from_raw(0),
		}
	}

	/// Create a process group wrapper attaching the command to an existing process group ID.
	pub fn attach_to(leader: u32) -> Self {
		Self {
			leader: Pid::from_raw(leader as _),
		}
	}
}

/// Wrapper for `Child` which ensures that all processes in the group are reaped.
#[derive(Debug)]
pub struct ProcessGroupChild {
	inner: Box<dyn ChildWrapper>,
	exit_status: ChildExitStatus,
	pgid: Pid,
}

impl ProcessGroupChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
	pub(crate) fn new(inner: Box<dyn ChildWrapper>, pgid: Pid) -> Self {
		Self {
			inner,
			exit_status: ChildExitStatus::Running,
			pgid,
		}
	}

	/// Get the process group ID of this child process.
	///
	/// See: [`man 'setpgid(2)'`](https://www.man7.org/linux/man-pages/man2/setpgid.2.html)
	pub fn pgid(&self) -> u32 {
		self.pgid.as_raw() as _
	}
}

impl CommandWrapper for ProcessGroup {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn pre_spawn(&mut self, command: &mut Command, _core: &CommandWrap) -> Result<()> {
		command.process_group(self.leader.as_raw());
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		inner: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let pgid = Pid::from_raw(i32::try_from(inner.id()).expect("Command PID > i32::MAX"));

		Ok(Box::new(ProcessGroupChild::new(inner, pgid)))
	}
}

impl ProcessGroupChild {
	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn signal_imp(&self, sig: Signal) -> Result<()> {
		killpg(self.pgid, sig).map_err(Error::from)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug"))]
	fn wait_imp(pgid: Pid, flag: WaitPidFlag) -> Result<ControlFlow<Option<ExitStatus>>> {
		// wait for processes in a loop until every process in this group has
		// exited (this ensures that we reap any zombies that may have been
		// created if the parent exited after spawning children, but didn't wait
		// for those children to exit)
		let mut parent_exit_status: Option<ExitStatus> = None;
		loop {
			// we can't use the safe wrapper directly because it doesn't return
			// the raw status, and we need it to convert to the std's ExitStatus
			let mut status: i32 = 0;
			match unsafe {
				libc::waitpid(-pgid.as_raw(), &mut status as *mut libc::c_int, flag.bits())
			} {
				0 => {
					// zero should only happen if WNOHANG was passed in,
					// and means that no processes have yet to exit
					return Ok(ControlFlow::Continue(()));
				}
				-1 => {
					match Errno::last() {
						Errno::ECHILD => {
							// no more children to reap; this is a graceful exit
							return Ok(ControlFlow::Break(parent_exit_status));
						}
						errno => {
							return Err(Error::from(errno));
						}
					}
				}
				pid => {
					// a process exited. was it the parent process that we
					// started? if so, collect the exit signal, otherwise we
					// reaped a zombie process and should continue looping
					if pgid == Pid::from_raw(pid) {
						parent_exit_status = Some(ExitStatus::from_raw(status));
					} else {
						// reaped a zombie child; keep looping
					}
				}
			};
		}
	}
}

impl ChildWrapper for ProcessGroupChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.inner()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.inner_mut()
	}
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner.into_inner()
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		self.signal_imp(Signal::SIGKILL)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Result<ExitStatus> {
		if let ChildExitStatus::Exited(status) = &self.exit_status {
			return Ok(*status);
		}

		// always wait for parent to exit first, as by the time it does,
		// it's likely that all its children have already been reaped.
		let status = self.inner.wait()?;
		self.exit_status = ChildExitStatus::Exited(status);

		// nevertheless, now wait and make sure we reap all children.
		let _ = Self::wait_imp(self.pgid, WaitPidFlag::empty())?;
		Ok(status)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		if let ChildExitStatus::Exited(status) = &self.exit_status {
			return Ok(Some(*status));
		}

		match Self::wait_imp(self.pgid, WaitPidFlag::WNOHANG)? {
			ControlFlow::Break(res) => {
				if let Some(status) = res {
					self.exit_status = ChildExitStatus::Exited(status);
				}
				Ok(res)
			}
			ControlFlow::Continue(()) => {
				let exited = self.inner.try_wait()?;
				if let Some(exited) = exited {
					self.exit_status = ChildExitStatus::Exited(exited);
				}
				Ok(exited)
			}
		}
	}

	fn signal(&self, sig: i32) -> Result<()> {
		self.signal_imp(Signal::try_from(sig)?)
	}
}
