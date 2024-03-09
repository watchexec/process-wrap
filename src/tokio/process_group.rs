use std::{
	future::Future,
	io::{Error, Result},
	ops::ControlFlow,
	os::unix::process::ExitStatusExt,
	process::{ExitStatus, Output},
};

use futures::future::try_join3;
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
	io::{AsyncRead, AsyncReadExt},
	process::{Child, Command},
	task::spawn_blocking,
};

use super::{TokioChildWrapper, TokioCommandWrapper};

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
	inner: Box<dyn TokioChildWrapper>,
	exit_status: ChildExitStatus,
	pgid: Pid,
}

#[derive(Debug)]
enum ChildExitStatus {
	Running,
	Exited(ExitStatus),
}

impl ProcessGroupChild {
	pub(crate) fn new(inner: Box<dyn TokioChildWrapper>, pgid: Pid) -> Self {
		Self {
			inner,
			exit_status: ChildExitStatus::Running,
			pgid,
		}
	}
}

impl TokioCommandWrapper for ProcessGroup {
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
		inner: Box<dyn TokioChildWrapper>,
	) -> Result<Box<dyn TokioChildWrapper>> {
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
	fn signal_imp(&self, sig: Signal) -> Result<()> {
		killpg(self.pgid, sig).map_err(Error::from)
	}

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

impl TokioChildWrapper for ProcessGroupChild {
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
			if let ChildExitStatus::Exited(status) = &self.exit_status {
				return Ok(*status);
			}

			const MAX_RETRY_ATTEMPT: usize = 10;
			let pgid = self.pgid;

			// always wait for parent to exit first, as by the time it does,
			// it's likely that all its children have already been reaped.
			let status = Box::into_pin(self.inner.wait()).await?;
			self.exit_status = ChildExitStatus::Exited(status);

			// nevertheless, now try reaping all children a few times...
			for _ in 1..MAX_RETRY_ATTEMPT {
				if Self::wait_imp(pgid, WaitPidFlag::WNOHANG)?.is_break() {
					return Ok(status);
				}
			}

			// ...finally, if there are some that are still alive,
			// block in the background to reap them fully.
			spawn_blocking(move || Self::wait_imp(pgid, WaitPidFlag::empty())).await??;
			Ok(status)
		})
	}

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

	fn wait_with_output(mut self: Box<Self>) -> Box<dyn Future<Output = Result<Output>>> {
		Box::new(async move {
			async fn read_to_end<A: AsyncRead + Unpin>(io: &mut Option<A>) -> Result<Vec<u8>> {
				let mut vec = Vec::new();
				if let Some(io) = io.as_mut() {
					io.read_to_end(&mut vec).await?;
				}
				Ok(vec)
			}

			let mut stdout_pipe = self.stdout().take();
			let mut stderr_pipe = self.stderr().take();

			let stdout_fut = read_to_end(&mut stdout_pipe);
			let stderr_fut = read_to_end(&mut stderr_pipe);

			let (status, stdout, stderr) =
				try_join3(Box::into_pin(self.wait()), stdout_fut, stderr_fut).await?;

			// Drop happens after `try_join` due to <https://github.com/tokio-rs/tokio/issues/4309>
			drop(stdout_pipe);
			drop(stderr_pipe);

			Ok(Output {
				status,
				stdout,
				stderr,
			})
		})
	}

	fn signal(&self, sig: Signal) -> Result<()> {
		self.signal_imp(sig)
	}
}
