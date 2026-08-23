use std::{
	ffi::{OsStr, OsString},
	io,
	path::Path,
	pin::Pin,
	task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{ChildWrapper, CommandWrap, CommandWrapper};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use unix as imp;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use unsupported as imp;

/// The character and pixel dimensions of a pseudo-terminal.
///
/// Character dimensions must both be nonzero. Pixel dimensions may be zero when they are unknown or
/// not meaningful to the caller. Sizes are validated by [`PtyCommand::spawn`] and
/// [`PtyResize::resize`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
	/// Character rows.
	pub rows: u16,
	/// Character columns.
	pub columns: u16,
	/// Pixel width, or zero when unspecified.
	pub pixel_width: u16,
	/// Pixel height, or zero when unspecified.
	pub pixel_height: u16,
}

impl PtySize {
	/// Construct a character-cell size with unspecified pixel dimensions.
	pub fn new(rows: u16, columns: u16) -> io::Result<Self> {
		let size = Self {
			rows,
			columns,
			pixel_width: 0,
			pixel_height: 0,
		};
		size.validate()?;
		Ok(size)
	}

	/// Set the optional pixel dimensions.
	pub fn with_pixels(mut self, pixel_width: u16, pixel_height: u16) -> Self {
		self.pixel_width = pixel_width;
		self.pixel_height = pixel_height;
		self
	}

	/// Validate that both character dimensions are nonzero.
	pub fn validate(self) -> io::Result<()> {
		if self.rows == 0 || self.columns == 0 {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"PTY rows and columns must both be nonzero",
			));
		}
		Ok(())
	}
}

impl Default for PtySize {
	fn default() -> Self {
		Self {
			rows: 24,
			columns: 80,
			pixel_width: 0,
			pixel_height: 0,
		}
	}
}

/// Options used when spawning a command in a pseudo-terminal.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PtyOptions {
	/// Initial terminal size.
	pub size: PtySize,
}

impl PtyOptions {
	/// Construct options with the requested initial terminal size.
	pub fn new(size: PtySize) -> Self {
		Self { size }
	}
}

#[derive(Debug)]
enum CommandArg {
	Regular(OsString),
	#[cfg(windows)]
	Raw(OsString),
}

#[derive(Debug)]
enum EnvChange {
	Set(OsString, OsString),
	Remove(OsString),
}

#[derive(Debug)]
struct CommandIntent {
	program: OsString,
	args: Vec<CommandArg>,
	env_clear: bool,
	env: Vec<EnvChange>,
	current_dir: Option<OsString>,
}

impl CommandIntent {
	fn command(&self) -> tokio::process::Command {
		let mut command = tokio::process::Command::new(&self.program);
		for arg in &self.args {
			match arg {
				CommandArg::Regular(arg) => {
					command.arg(arg);
				}
				#[cfg(windows)]
				CommandArg::Raw(arg) => {
					command.raw_arg(arg);
				}
			}
		}

		if self.env_clear {
			command.env_clear();
		}
		for change in &self.env {
			match change {
				EnvChange::Set(key, value) => {
					command.env(key, value);
				}
				EnvChange::Remove(key) => {
					command.env_remove(key);
				}
			}
		}
		if let Some(directory) = &self.current_dir {
			command.current_dir(directory);
		}
		command
	}
}

/// A tracked command builder for pseudo-terminal spawning.
///
/// The builder owns the complete portable command intent instead of exposing unrestricted mutable
/// access to the underlying Tokio command. This lets platform backends preserve arguments,
/// environment operations, the working directory, and wrapper registration exactly.
#[derive(Debug)]
pub struct PtyCommand {
	command: CommandWrap,
	intent: CommandIntent,
}

impl PtyCommand {
	/// Construct a pseudo-terminal command builder for a program.
	pub fn new(program: impl AsRef<OsStr>) -> Self {
		let program = program.as_ref().to_os_string();
		Self {
			command: CommandWrap::with_new(&program, |_| {}),
			intent: CommandIntent {
				program,
				args: Vec::new(),
				env_clear: false,
				env: Vec::new(),
				current_dir: None,
			},
		}
	}

	/// Replace the program to execute.
	pub fn program(&mut self, program: impl AsRef<OsStr>) -> &mut Self {
		self.intent.program = program.as_ref().to_os_string();
		self
	}

	/// Append one argument using the platform command's normal argument semantics.
	pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		self.intent
			.args
			.push(CommandArg::Regular(arg.as_ref().to_os_string()));
		self
	}

	/// Append arguments using the platform command's normal argument semantics.
	pub fn args<I, S>(&mut self, args: I) -> &mut Self
	where
		I: IntoIterator<Item = S>,
		S: AsRef<OsStr>,
	{
		for arg in args {
			self.arg(arg);
		}
		self
	}

	/// Append a raw command-line fragment on Windows.
	///
	/// Raw fragments are preserved in their registration order relative to regular arguments.
	#[cfg(windows)]
	pub fn raw_arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		self.intent
			.args
			.push(CommandArg::Raw(arg.as_ref().to_os_string()));
		self
	}

	/// Set an environment variable for the child.
	pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
		self.intent.env.push(EnvChange::Set(
			key.as_ref().to_os_string(),
			value.as_ref().to_os_string(),
		));
		self
	}

	/// Set environment variables for the child.
	pub fn envs<I, K, V>(&mut self, variables: I) -> &mut Self
	where
		I: IntoIterator<Item = (K, V)>,
		K: AsRef<OsStr>,
		V: AsRef<OsStr>,
	{
		for (key, value) in variables {
			self.env(key, value);
		}
		self
	}

	/// Remove an inherited or explicitly set environment variable.
	pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
		self.intent
			.env
			.push(EnvChange::Remove(key.as_ref().to_os_string()));
		self
	}

	/// Clear the inherited environment and all prior environment changes.
	pub fn env_clear(&mut self) -> &mut Self {
		self.intent.env_clear = true;
		self.intent.env.clear();
		self
	}

	/// Set the child's working directory.
	pub fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
		self.intent.current_dir = Some(directory.as_ref().as_os_str().to_os_string());
		self
	}

	/// Register a process wrapper.
	pub fn wrap<W: CommandWrapper + 'static>(&mut self, wrapper: W) -> &mut Self {
		self.command.wrap(wrapper);
		self
	}

	/// Spawn the command attached to a pseudo-terminal.
	///
	/// This API never falls back to ordinary pipes. On a target without a backend it returns
	/// [`io::ErrorKind::Unsupported`].
	pub fn spawn(
		&mut self,
		options: PtyOptions,
	) -> io::Result<(Box<dyn ChildWrapper>, PtyController)> {
		imp::spawn(self, options)
	}

	fn rebuild_command(&mut self) {
		*self.command.command_mut() = self.intent.command();
	}
}

/// The asynchronous input side of a pseudo-terminal.
///
/// Input and output are strong owners of the same bidirectional PTY master. Shutting down or
/// dropping only this input object releases its owner but cannot close the master or produce child
/// EOF while [`PtyOutput`] still exists. There is no independent transport half-close; send the
/// terminal's VEOF control character when that is the desired terminal policy.
#[derive(Debug)]
pub struct PtyInput {
	inner: imp::Input,
}

impl AsyncWrite for PtyInput {
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &[u8],
	) -> Poll<io::Result<usize>> {
		Pin::new(&mut self.inner).poll_write(cx, buffer)
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_flush(cx)
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_shutdown(cx)
	}
}

/// The asynchronous merged output side of a pseudo-terminal.
///
/// This is a strong owner of the shared bidirectional PTY master. Output EOF means that every slave
/// descriptor has closed; it is independent from waiting for the direct child because a descendant
/// may retain the slave after that child exits.
#[derive(Debug)]
pub struct PtyOutput {
	inner: imp::Output,
}

impl AsyncRead for PtyOutput {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buffer: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buffer)
	}
}

/// A cloneable weak handle for resizing a pseudo-terminal.
///
/// Resize handles do not keep the PTY master alive.
#[derive(Clone, Debug)]
pub struct PtyResize {
	inner: imp::Resize,
}

impl PtyResize {
	/// Change the terminal size.
	///
	/// Returns [`io::ErrorKind::BrokenPipe`] after both strong I/O owners have been dropped.
	pub fn resize(&self, size: PtySize) -> io::Result<()> {
		size.validate()?;
		self.inner.resize(size)
	}
}

/// The I/O and resize controls for a spawned pseudo-terminal.
///
/// [`PtyInput`] and [`PtyOutput`] each strongly own one shared bidirectional master. The terminal
/// hangs up only after both owners are gone; dropping either one alone is not a half-close. A
/// [`PtyResize`] is weak and never keeps the terminal alive. There is deliberately no clonable
/// force-close handle, parent-terminal raw mode, byte relay, key handling, VT parsing, scrollback,
/// or pager policy in this transport.
///
/// Process supervision and PTY draining are separate lifecycles. Waiting for the direct child does
/// not imply output EOF, because descendants can retain slave descriptors.
#[derive(Debug)]
pub struct PtyController {
	input: PtyInput,
	output: PtyOutput,
	resize: PtyResize,
}

impl PtyController {
	#[allow(dead_code)]
	fn new(input: imp::Input, output: imp::Output, resize: imp::Resize) -> Self {
		Self {
			input: PtyInput { inner: input },
			output: PtyOutput { inner: output },
			resize: PtyResize { inner: resize },
		}
	}

	/// Borrow the input side.
	pub fn input(&mut self) -> &mut PtyInput {
		&mut self.input
	}

	/// Borrow the merged output side.
	pub fn output(&mut self) -> &mut PtyOutput {
		&mut self.output
	}

	/// Clone the weak resize handle.
	pub fn resizer(&self) -> PtyResize {
		self.resize.clone()
	}

	/// Split the controller into independently owned input, output, and resize handles.
	pub fn into_parts(self) -> (PtyInput, PtyOutput, PtyResize) {
		(self.input, self.output, self.resize)
	}
}
