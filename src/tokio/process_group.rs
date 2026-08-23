#[cfg(feature = "pty")]
use std::io::ErrorKind;
use std::{
	future::Future,
	io::{Error, Result},
	ops::ControlFlow,
	os::unix::process::ExitStatusExt,
	pin::Pin,
	process::ExitStatus,
};

use nix::{
	errno::Errno,
	libc,
	sys::{
		signal::{Signal, killpg},
		wait::WaitPidFlag,
	},
	unistd::Pid,
};
use tokio::{process::Command, task::spawn_blocking};
#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::ChildExitStatus;

#[cfg(feature = "pty")]
use super::pty::PtyMarker;
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
/// With a supported Unix `PtyCommand`, [`leader`](Self::leader) leaves session and process-group
/// creation to the PTY backend while retaining group-aware child supervision. Attaching a new PTY
/// session to an existing group is invalid, so [`attach_to`](Self::attach_to) causes PTY spawning to
/// return [`std::io::ErrorKind::InvalidInput`].
///
/// This wrapper provides a child wrapper: [`ProcessGroupChild`].
#[derive(Clone, Copy, Debug)]
pub struct ProcessGroup {
	leader: Pid,
	#[cfg(feature = "pty")]
	attach: bool,
}

impl ProcessGroup {
	/// Create a process group wrapper setting up a new process group with the command as the leader.
	pub fn leader() -> Self {
		Self {
			leader: Pid::from_raw(0),
			#[cfg(feature = "pty")]
			attach: false,
		}
	}

	/// Create a process group wrapper attaching the command to an existing process group ID.
	pub fn attach_to(leader: u32) -> Self {
		Self {
			leader: Pid::from_raw(leader as i32),
			#[cfg(feature = "pty")]
			attach: true,
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
		#[cfg(feature = "pty")]
		if _core.has_wrap::<PtyMarker>() {
			#[cfg(feature = "process-session")]
			if _core.has_wrap::<super::ProcessSession>() {
				return Err(Error::new(
					ErrorKind::InvalidInput,
					"ProcessGroup and ProcessSession cannot both be used with a PTY",
				));
			}
			if self.attach {
				return Err(Error::new(
					ErrorKind::InvalidInput,
					"ProcessGroup::attach_to cannot be used with a PTY",
				));
			}
			return Ok(());
		}

		command.process_group(self.leader.as_raw());
		Ok(())
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wrap_child(
		&mut self,
		inner: Box<dyn ChildWrapper>,
		_core: &CommandWrap,
	) -> Result<Box<dyn ChildWrapper>> {
		let pgid = Pid::from_raw(
			i32::try_from(
				inner
					.id()
					.expect("Command was reaped before we could read its PID"),
			)
			.expect("Command PID > i32::MAX"),
		);

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
		self.inner.as_ref()
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn start_kill(&mut self) -> Result<()> {
		self.signal_imp(Signal::SIGKILL)
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
		Box::pin(async {
			if let ChildExitStatus::Exited(status) = &self.exit_status {
				return Ok(*status);
			}

			const MAX_RETRY_ATTEMPT: usize = 10;
			let pgid = self.pgid;

			// always wait for parent to exit first, as by the time it does,
			// it's likely that all its children have already been reaped.
			let status = self.inner.wait().await?;
			self.exit_status = ChildExitStatus::Exited(status);

			// nevertheless, now try reaping all children a few times...
			for _ in 1..MAX_RETRY_ATTEMPT {
				if Self::wait_imp(pgid, WaitPidFlag::WNOHANG)?.is_break() {
					return Ok(status);
				}
			}

			// ...finally, if there are some that are still alive,
			// block in the background to reap them fully.
			let _ = spawn_blocking(move || Self::wait_imp(pgid, WaitPidFlag::empty())).await??;
			Ok(status)
		})
	}

	#[cfg_attr(feature = "tracing", instrument(level = "debug", skip(self)))]
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		if let ChildExitStatus::Exited(status) = &self.exit_status {
			return Ok(Some(*status));
		}

		let exited = self.inner.try_wait()?;
		if let Some(status) = exited {
			self.exit_status = ChildExitStatus::Exited(status);
			// The native child must reap the leader so its own cached state remains synchronized.
			// Once that has happened, opportunistically reap any exited descendants in the group.
			let _ = Self::wait_imp(self.pgid, WaitPidFlag::WNOHANG)?;
		}
		Ok(exited)
	}

	fn signal(&self, sig: i32) -> Result<()> {
		self.signal_imp(Signal::try_from(sig)?)
	}
}
