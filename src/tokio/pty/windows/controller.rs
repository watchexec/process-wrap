//! Shared Windows PTY controller ownership and Tokio named-pipe I/O.

use std::{
	io,
	os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
	pin::Pin,
	sync::{Arc, Weak},
	task::{Context, Poll},
};

use tokio::{
	io::{AsyncRead, AsyncWrite, ReadBuf},
	net::windows::named_pipe::NamedPipeClient,
};
use windows::Win32::{
	Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
	System::Threading::GetCurrentProcess,
};

use super::{super::PtySize, console::PseudoConsole};

type SharedMaster = Arc<Master>;

#[derive(Debug)]
struct Master {
	_input_lifetime: OwnedHandle,
	_output_lifetime: OwnedHandle,
	console: PseudoConsole,
}

#[derive(Debug)]
pub(in crate::tokio::pty) struct Input {
	pipe: Option<NamedPipeClient>,
	master: Option<SharedMaster>,
}

impl AsyncWrite for Input {
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &[u8],
	) -> Poll<io::Result<usize>> {
		let Some(pipe) = self.pipe.as_mut() else {
			return Poll::Ready(Err(closed()));
		};
		Pin::new(pipe).poll_write(cx, buffer)
	}

	fn poll_write_vectored(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffers: &[io::IoSlice<'_>],
	) -> Poll<io::Result<usize>> {
		let Some(pipe) = self.pipe.as_mut() else {
			return Poll::Ready(Err(closed()));
		};
		Pin::new(pipe).poll_write_vectored(cx, buffers)
	}

	fn is_write_vectored(&self) -> bool {
		self.pipe
			.as_ref()
			.is_some_and(AsyncWrite::is_write_vectored)
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		let Some(pipe) = self.pipe.as_mut() else {
			return Poll::Ready(Err(closed()));
		};
		Pin::new(pipe).poll_flush(cx)
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		let Some(pipe) = self.pipe.as_mut() else {
			return Poll::Ready(Ok(()));
		};
		match Pin::new(pipe).poll_shutdown(cx) {
			Poll::Ready(Ok(())) => {
				self.pipe.take();
				self.master.take();
				Poll::Ready(Ok(()))
			}
			other => other,
		}
	}
}

#[derive(Debug)]
pub(in crate::tokio::pty) struct Output {
	pipe: NamedPipeClient,
	_master: SharedMaster,
}

impl AsyncRead for Output {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.pipe).poll_read(cx, buffer)
	}
}

#[derive(Clone, Debug)]
pub(in crate::tokio::pty) struct Resize {
	master: Weak<Master>,
}

impl Resize {
	pub(in crate::tokio::pty) fn resize(&self, size: PtySize) -> io::Result<()> {
		let master = self.master.upgrade().ok_or_else(closed)?;
		master.console.resize(size)
	}
}

pub(super) fn controller(
	console: PseudoConsole,
	input: NamedPipeClient,
	output: NamedPipeClient,
) -> io::Result<(Input, Output, Resize)> {
	let input_lifetime = duplicate(HANDLE(input.as_raw_handle()))?;
	let output_lifetime = duplicate(HANDLE(output.as_raw_handle()))?;
	let master = Arc::new(Master {
		_input_lifetime: input_lifetime,
		_output_lifetime: output_lifetime,
		console,
	});
	let resize = Resize {
		master: Arc::downgrade(&master),
	};
	let input = Input {
		pipe: Some(input),
		master: Some(Arc::clone(&master)),
	};
	let output = Output {
		pipe: output,
		_master: master,
	};
	Ok((input, output, resize))
}

fn duplicate(handle: HANDLE) -> io::Result<OwnedHandle> {
	// SAFETY: the current-process pseudo-handle and source named-pipe handle are live, duplicate points
	// to writable storage, and ownership of the non-inheritable result is transferred exactly once.
	unsafe {
		let process = GetCurrentProcess();
		let mut duplicate = HANDLE::default();
		DuplicateHandle(
			process,
			handle,
			process,
			&mut duplicate,
			0,
			false,
			DUPLICATE_SAME_ACCESS,
		)
		.map_err(io::Error::other)?;
		Ok(OwnedHandle::from_raw_handle(duplicate.0))
	}
}

fn closed() -> io::Error {
	io::Error::new(io::ErrorKind::BrokenPipe, "PTY master is closed")
}
