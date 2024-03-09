use std::{
	future::Future,
	io::{Error, Result},
	ops::ControlFlow,
	os::unix::process::ExitStatusExt,
	pin::pin,
	process::ExitStatus,
};

use nix::{
	errno::Errno,
	libc,
	sys::{
		signal::{killpg, Signal},
		wait::WaitPidFlag,
	},
	unistd::{setpgid, Pid},
};
use tokio::{
	process::{Child, Command},
	task::spawn_blocking,
};

#[derive(Debug, Clone)]
pub struct ProcessGroup {
	leader: Pid,
}

impl ProcessGroup {
	pub fn leader() -> Self {
		Self {
			leader: Pid::from_raw(0),
		}
	}

	pub fn attach_to(leader: Pid) -> Self {
		Self { leader }
	}
}

#[derive(Debug)]
pub struct ProcessGroupChild {
	inner: Box<dyn super::core::TokioChildWrapper>,
	pgid: Pid,
}

impl super::core::TokioCommandWrapper for ProcessGroup {
	fn pre_spawn(&mut self, command: &mut Command) -> Result<()> {
		#[cfg(tokio_unstable)]
		{
			command.process_group(self.leader.as_raw());
		}

		#[cfg(not(tokio_unstable))]
		let leader = self.leader;
		unsafe {
			command.pre_exec(move || {
				setpgid(Pid::this(), leader)
					.map_err(Error::from)
					.map(|_| ())
			});
		}

		Ok(())
	}

	fn wrap_child(
		&mut self,
		inner: Box<dyn super::core::TokioChildWrapper>,
	) -> Result<Box<dyn super::core::TokioChildWrapper>> {
		let pgid = Pid::from_raw(
			i32::try_from(
				inner
					.id()
					.expect("Command was reaped before we could read its PID"),
			)
			.expect("Command PID > i32::MAX"),
		);

		Ok(Box::new(ProcessGroupChild { inner, pgid }))
	}
}

impl ProcessGroupChild {
	fn signal_imp(&self, sig: Signal) -> Result<()> {
		killpg(self.pgid, sig).map_err(Error::from)
	}

	fn wait_imp(pgid: i32, flag: WaitPidFlag) -> Result<ControlFlow<Option<ExitStatus>>> {
		// Wait for processes in a loop until every process in this
		// process group has exited (this ensures that we reap any
		// zombies that may have been created if the parent exited after
		// spawning children, but didn't wait for those children to
		// exit).
		let mut parent_exit_status: Option<ExitStatus> = None;
		loop {
			// we can't use the safe wrapper directly because it doesn't
			// return the raw status, and we need it to convert to the
			// std's ExitStatus.
			let mut status: i32 = 0;
			match unsafe { libc::waitpid(-pgid, &mut status as *mut libc::c_int, flag.bits()) } {
				0 => {
					// Zero should only happen if WNOHANG was passed in,
					// and means that no processes have yet to exit.
					return Ok(ControlFlow::Continue(()));
				}
				-1 => {
					match Errno::last() {
						Errno::ECHILD => {
							// No more children to reap; this is a
							// graceful exit.
							return Ok(ControlFlow::Break(parent_exit_status));
						}
						errno => {
							return Err(Error::from(errno));
						}
					}
				}
				pid => {
					// *A* process exited. Was it the parent process
					// that we started? If so, collect the exit signal,
					// otherwise we reaped a zombie process and should
					// continue in the loop.
					if pgid == pid {
						parent_exit_status = Some(ExitStatus::from_raw(status));
					} else {
						// Reaped a zombie child; keep looping.
					}
				}
			};
		}
	}
}

impl super::core::TokioChildWrapper for ProcessGroupChild {
	fn inner(&self) -> &Child {
		self.inner.inner()
	}
	fn inner_mut(&mut self) -> &mut Child {
		self.inner.inner_mut()
	}
	fn into_inner(self: Box<Self>) -> Child {
		self.inner.into_inner()
	}

	fn start_kill(&mut self) -> Result<()> {
		self.signal_imp(Signal::SIGKILL)
	}

	fn kill(&mut self) -> Box<dyn Future<Output = Result<()>> + '_> {
		Box::new(async {
			self.start_kill()?;
			Box::into_pin(self.wait()).await?;
			Ok(())
		})
	}

	fn wait(&mut self) -> Box<dyn Future<Output = Result<ExitStatus>> + '_> {
		Box::new(async {
			const MAX_RETRY_ATTEMPT: usize = 10;

			// Always wait for parent to exit first.
			//
			// It's likely that all its children has already exited and reaped by
			// the time the parent exits.
			let status = Box::into_pin(self.inner.wait()).await?;

			let pgid = self.pgid.as_raw();

			// Try reaping all children, if there are some that are still alive after
			// several attempts, then spawn a blocking task to reap them.
			for retry_attempt in 1..=MAX_RETRY_ATTEMPT {
				if Self::wait_imp(pgid, WaitPidFlag::WNOHANG)?.is_break() {
					break;
				} else if retry_attempt == MAX_RETRY_ATTEMPT {
					pin!(spawn_blocking(move || Self::wait_imp(
						pgid,
						WaitPidFlag::empty()
					)))
					.await??;
				}
			}

			Ok(status)
		})
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		match Self::wait_imp(self.pgid.as_raw(), WaitPidFlag::WNOHANG)? {
			ControlFlow::Break(res) => Ok(res),
			ControlFlow::Continue(()) => self.inner.try_wait(),
		}
	}

	fn signal(&self, sig: Signal) -> Result<()> {
		self.signal_imp(sig)
	}
}
