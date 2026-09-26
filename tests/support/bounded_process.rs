use std::{
	io::{self, Read},
	process::{Child, ExitStatus, Output, Stdio},
	sync::mpsc,
	thread::{self, JoinHandle},
	time::{Duration, Instant},
};

const REAP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderStartFailure {
	First,
	Second,
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
	fn new(child: Child) -> Self {
		Self(Some(child))
	}

	fn child_mut(&mut self) -> &mut Child {
		self.0
			.as_mut()
			.expect("an armed subprocess guard owns its child")
	}

	fn terminate_and_reap(&mut self) -> io::Result<ExitStatus> {
		let child = self.child_mut();
		let kill_error = match child.kill() {
			Ok(()) => None,
			Err(error) if error.kind() == io::ErrorKind::InvalidInput => None,
			Err(error) => Some(error),
		};
		let deadline = Instant::now() + REAP_TIMEOUT;
		loop {
			match child.try_wait() {
				Ok(Some(status)) => return Ok(status),
				Ok(None) => {}
				Err(error) => return Err(kill_error.unwrap_or(error)),
			}
			if Instant::now() >= deadline {
				return Err(kill_error.unwrap_or_else(|| {
					io::Error::new(
						io::ErrorKind::TimedOut,
						"isolated subprocess was not reaped after termination",
					)
				}));
			}
			thread::sleep(POLL_INTERVAL);
		}
	}

	fn disarm(&mut self) {
		self.0.take();
	}
}

impl Drop for ChildGuard {
	fn drop(&mut self) {
		if self.0.is_some() {
			let _ = self.terminate_and_reap();
		}
	}
}

struct PipeReader {
	stream: &'static str,
	receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
	handle: Option<JoinHandle<()>>,
}

impl PipeReader {
	fn finish(mut self, deadline: Instant) -> io::Result<Vec<u8>> {
		let remaining = deadline.saturating_duration_since(Instant::now());
		let result = match self.receiver.recv_timeout(remaining) {
			Ok(result) => result,
			Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
				io::ErrorKind::TimedOut,
				format!("did not drain isolated {} before the deadline", self.stream),
			)),
			Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(format!(
				"isolated {} reader stopped without a result",
				self.stream
			))),
		};

		let handle = self
			.handle
			.as_ref()
			.expect("an unfinished pipe reader owns its thread");
		while !handle.is_finished() && Instant::now() < deadline {
			thread::sleep(POLL_INTERVAL);
		}
		if !handle.is_finished() {
			return Err(io::Error::new(
				io::ErrorKind::TimedOut,
				format!("isolated {} reader did not stop", self.stream),
			));
		}
		if self
			.handle
			.take()
			.expect("a finished pipe reader owns its thread")
			.join()
			.is_err()
		{
			return Err(io::Error::other(format!(
				"isolated {} reader panicked",
				self.stream
			)));
		}
		result
	}
}

fn start_reader(
	mut pipe: impl Read + Send + 'static,
	stream: &'static str,
	failure: Option<ReaderStartFailure>,
	position: ReaderStartFailure,
) -> io::Result<PipeReader> {
	if failure == Some(position) {
		let ordinal = match position {
			ReaderStartFailure::First => "first",
			ReaderStartFailure::Second => "second",
		};
		return Err(io::Error::other(format!(
			"injected {ordinal} reader setup failure"
		)));
	}

	let (sender, receiver) = mpsc::channel();
	let handle = thread::Builder::new()
		.name(format!("process-wrap-{stream}-reader"))
		.spawn(move || {
			let mut bytes = Vec::new();
			let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
			let _ = sender.send(result);
		})
		.map_err(|error| {
			io::Error::new(
				error.kind(),
				format!("could not start isolated {stream} reader: {error}"),
			)
		})?;
	Ok(PipeReader {
		stream,
		receiver,
		handle: Some(handle),
	})
}

fn finish_readers(
	status: ExitStatus,
	stdout: PipeReader,
	stderr: PipeReader,
) -> io::Result<Output> {
	let deadline = Instant::now() + REAP_TIMEOUT;
	let stdout = stdout.finish(deadline);
	let stderr = stderr.finish(deadline);
	Ok(Output {
		status,
		stdout: stdout?,
		stderr: stderr?,
	})
}

fn preserve_after_cleanup(
	child: &mut ChildGuard,
	stdout: Option<PipeReader>,
	stderr: Option<PipeReader>,
	primary: io::Error,
) -> io::Error {
	let process_cleanup = child.terminate_and_reap();
	if process_cleanup.is_ok() {
		child.disarm();
	}
	let deadline = Instant::now() + REAP_TIMEOUT;
	let stdout_cleanup = stdout.map(|reader| reader.finish(deadline));
	let stderr_cleanup = stderr.map(|reader| reader.finish(deadline));
	let cleanup_error = process_cleanup
		.err()
		.or_else(|| stdout_cleanup.and_then(Result::err))
		.or_else(|| stderr_cleanup.and_then(Result::err));
	match cleanup_error {
		Some(cleanup) => io::Error::new(
			primary.kind(),
			format!("{primary}; subprocess cleanup also failed: {cleanup}"),
		),
		None => primary,
	}
}

pub(crate) fn run(
	mut command: std::process::Command,
	timeout: Duration,
	reader_failure: Option<ReaderStartFailure>,
) -> io::Result<(Output, bool)> {
	let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"subprocess timeout is too large",
		)
	})?;
	let spawned = command
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()?;
	let mut child = ChildGuard::new(spawned);

	let stdout = child
		.child_mut()
		.stdout
		.take()
		.ok_or_else(|| io::Error::other("isolated subprocess stdout was not captured"))?;
	let stderr = child
		.child_mut()
		.stderr
		.take()
		.ok_or_else(|| io::Error::other("isolated subprocess stderr was not captured"))?;

	let stdout = match start_reader(stdout, "stdout", reader_failure, ReaderStartFailure::First) {
		Ok(reader) => reader,
		Err(error) => {
			drop(stderr);
			return Err(preserve_after_cleanup(&mut child, None, None, error));
		}
	};
	let stderr = match start_reader(stderr, "stderr", reader_failure, ReaderStartFailure::Second) {
		Ok(reader) => reader,
		Err(error) => {
			return Err(preserve_after_cleanup(
				&mut child,
				Some(stdout),
				None,
				error,
			));
		}
	};

	loop {
		let status = match child.child_mut().try_wait() {
			Ok(status) => status,
			Err(error) => {
				return Err(preserve_after_cleanup(
					&mut child,
					Some(stdout),
					Some(stderr),
					error,
				));
			}
		};
		if let Some(status) = status {
			let output = finish_readers(status, stdout, stderr)?;
			child.disarm();
			return Ok((output, false));
		}
		if Instant::now() >= deadline {
			let status = match child.terminate_and_reap() {
				Ok(status) => status,
				Err(error) => {
					return Err(preserve_after_cleanup(
						&mut child,
						Some(stdout),
						Some(stderr),
						error,
					));
				}
			};
			let output = finish_readers(status, stdout, stderr)?;
			child.disarm();
			return Ok((output, true));
		}
		thread::sleep(POLL_INTERVAL);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reader_start_failures_terminate_reap_and_join_before_returning() {
		const MARKER_ENV: &str = "PROCESS_WRAP_READER_SETUP_MARKER";

		for failure in [ReaderStartFailure::First, ReaderStartFailure::Second] {
			let directory = tempfile::tempdir().expect("create setup-failure directory");
			let marker = directory.path().join("delayed-marker");

			#[cfg(unix)]
			let mut command = {
				let mut command = std::process::Command::new("sh");
				command.args(["-c", "sleep 0.3; : > \"$PROCESS_WRAP_READER_SETUP_MARKER\""]);
				command
			};
			#[cfg(windows)]
			let mut command = {
				let mut command = std::process::Command::new("cmd.exe");
				command.args([
					"/D",
					"/S",
					"/C",
					"ping 127.0.0.1 -n 2 >NUL & type NUL > \"%PROCESS_WRAP_READER_SETUP_MARKER%\"",
				]);
				command
			};
			command.env(MARKER_ENV, &marker);

			let error = run(command, REAP_TIMEOUT, Some(failure))
				.expect_err("reader setup must fail at the injected position");
			let ordinal = match failure {
				ReaderStartFailure::First => "first",
				ReaderStartFailure::Second => "second",
			};
			assert_eq!(
				error.to_string(),
				format!("injected {ordinal} reader setup failure"),
				"the injected error is returned only after cleanup succeeds"
			);
			#[cfg(unix)]
			thread::sleep(Duration::from_millis(750));
			#[cfg(windows)]
			thread::sleep(Duration::from_millis(1_500));
			assert!(
				!marker.exists(),
				"a setup failure must terminate the child before its delayed marker"
			);
		}
	}
}
