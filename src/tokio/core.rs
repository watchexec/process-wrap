use std::{
	future::Future,
	io::Result,
	process::{ExitStatus, Output},
};

use futures::future::try_join3;
#[cfg(unix)]
use nix::{
	sys::signal::{kill, Signal},
	unistd::Pid,
};
use tokio::{
	io::{AsyncRead, AsyncReadExt},
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};

crate::generic_wrap::Wrap!(
	TokioCommandWrap,
	Command,
	TokioCommandWrapper,
	Child,
	TokioChildWrapper,
	|child| child
);

pub trait TokioChildWrapper: std::fmt::Debug + Send + Sync {
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

	fn id(&self) -> Option<u32> {
		self.inner().id()
	}

	fn kill(&mut self) -> Box<dyn Future<Output = Result<()>> + '_> {
		Box::new(async {
			self.start_kill()?;
			Box::into_pin(self.wait()).await?;
			Ok(())
		})
	}

	fn start_kill(&mut self) -> Result<()> {
		self.inner_mut().start_kill()
	}

	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	fn wait(&mut self) -> Box<dyn Future<Output = Result<ExitStatus>> + '_> {
		Box::new(self.inner_mut().wait())
	}

	fn wait_with_output(mut self: Box<Self>) -> Box<dyn Future<Output = Result<Output>>>
	where
		Self: 'static,
	{
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

	#[cfg(unix)]
	fn signal(&self, sig: Signal) -> Result<()> {
		if let Some(id) = self.id() {
			kill(
				Pid::from_raw(i32::try_from(id).map_err(std::io::Error::other)?),
				sig,
			)
			.map_err(std::io::Error::from)
		} else {
			Ok(())
		}
	}
}

impl TokioChildWrapper for Child {
	fn inner(&self) -> &Child {
		self
	}
	fn inner_mut(&mut self) -> &mut Child {
		self
	}
	fn into_inner(self: Box<Self>) -> Child {
		*self
	}
}
