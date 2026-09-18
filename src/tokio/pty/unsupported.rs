use std::{
	io,
	pin::Pin,
	task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{ProviderProduct, PtySize, SpawnAttempt};

#[derive(Debug)]
pub(super) struct Input;

impl AsyncWrite for Input {
	fn poll_write(
		self: Pin<&mut Self>,
		_cx: &mut Context<'_>,
		_buffer: &[u8],
	) -> Poll<io::Result<usize>> {
		Poll::Ready(Err(unsupported()))
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Err(unsupported()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Err(unsupported()))
	}
}

#[derive(Debug)]
pub(super) struct Output;

impl AsyncRead for Output {
	fn poll_read(
		self: Pin<&mut Self>,
		_cx: &mut Context<'_>,
		_buffer: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Poll::Ready(Err(unsupported()))
	}
}

#[derive(Clone, Debug)]
pub(super) struct Resize;

impl Resize {
	pub(super) fn resize(&self, _size: PtySize) -> io::Result<()> {
		Err(unsupported())
	}
}

pub(super) fn check_available() -> io::Result<()> {
	Err(unsupported())
}

pub(super) fn spawn(_attempt: &mut SpawnAttempt, _size: PtySize) -> io::Result<ProviderProduct> {
	Err(unsupported())
}

fn unsupported() -> io::Error {
	io::Error::new(
		io::ErrorKind::Unsupported,
		"pseudo-terminal spawning is unsupported on this platform",
	)
}
