#![cfg_attr(not(any(feature = "std", feature = "tokio1")), allow(dead_code))]

use std::{
	any::Any,
	ffi::{OsStr, OsString},
	fmt,
	marker::PhantomData,
	path::{Path, PathBuf},
	process::Stdio,
};

/// Blocking standard-library process frontend.
#[doc(hidden)]
#[derive(Debug)]
pub struct Blocking;

/// Asynchronous Tokio process frontend.
#[doc(hidden)]
#[derive(Debug)]
pub struct Tokio1;

mod private {
	pub trait Sealed {}

	#[cfg(feature = "std")]
	impl Sealed for super::Blocking {}

	#[cfg(feature = "tokio1")]
	impl Sealed for super::Tokio1 {}
}

/// Backend implementation detail for [`Command`].
#[doc(hidden)]
pub trait Backend: private::Sealed + 'static {
	/// The frontend's native command type.
	type NativeCommand: NativeCommand;

	/// Create the frontend-specific wrapper registry.
	fn new_registry() -> Box<dyn Any + Send + Sync>;
}

/// Native command operations shared by the supported frontends.
#[doc(hidden)]
pub trait NativeCommand: fmt::Debug + Sized + 'static {
	/// Create a command for `program`.
	fn new(program: &OsStr) -> Self;

	/// Add a regular argument.
	fn arg(&mut self, arg: &OsStr);

	/// Add a raw Windows command-line fragment.
	#[cfg(windows)]
	fn raw_arg(&mut self, arg: &OsStr);

	/// Set an environment variable.
	fn env(&mut self, key: &OsStr, value: &OsStr);

	/// Remove an environment variable.
	fn env_remove(&mut self, key: &OsStr);

	/// Clear explicitly configured and inherited environment variables.
	fn env_clear(&mut self);

	/// Set the current directory.
	fn current_dir(&mut self, dir: &Path);

	/// Configure standard input.
	fn stdin(&mut self, stdio: Stdio);

	/// Configure standard output.
	fn stdout(&mut self, stdio: Stdio);

	/// Configure standard error.
	fn stderr(&mut self, stdio: Stdio);

	/// Get the program.
	fn get_program(&self) -> &OsStr;

	/// Get the arguments.
	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_>;

	/// Get the current directory.
	fn get_current_dir(&self) -> Option<&Path>;
}

#[cfg(feature = "std")]
impl NativeCommand for std::process::Command {
	fn new(program: &OsStr) -> Self {
		Self::new(program)
	}

	fn arg(&mut self, arg: &OsStr) {
		self.arg(arg);
	}

	#[cfg(windows)]
	fn raw_arg(&mut self, arg: &OsStr) {
		use std::os::windows::process::CommandExt;
		CommandExt::raw_arg(self, arg);
	}

	fn env(&mut self, key: &OsStr, value: &OsStr) {
		self.env(key, value);
	}

	fn env_remove(&mut self, key: &OsStr) {
		self.env_remove(key);
	}

	fn env_clear(&mut self) {
		self.env_clear();
	}

	fn current_dir(&mut self, dir: &Path) {
		self.current_dir(dir);
	}

	fn stdin(&mut self, stdio: Stdio) {
		self.stdin(stdio);
	}

	fn stdout(&mut self, stdio: Stdio) {
		self.stdout(stdio);
	}

	fn stderr(&mut self, stdio: Stdio) {
		self.stderr(stdio);
	}

	fn get_program(&self) -> &OsStr {
		self.get_program()
	}

	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		Box::new(self.get_args())
	}

	fn get_current_dir(&self) -> Option<&Path> {
		self.get_current_dir()
	}
}

#[cfg(feature = "tokio1")]
impl NativeCommand for tokio::process::Command {
	fn new(program: &OsStr) -> Self {
		Self::new(program)
	}

	fn arg(&mut self, arg: &OsStr) {
		self.arg(arg);
	}

	#[cfg(windows)]
	fn raw_arg(&mut self, arg: &OsStr) {
		self.raw_arg(arg);
	}

	fn env(&mut self, key: &OsStr, value: &OsStr) {
		self.env(key, value);
	}

	fn env_remove(&mut self, key: &OsStr) {
		self.env_remove(key);
	}

	fn env_clear(&mut self) {
		self.env_clear();
	}

	fn current_dir(&mut self, dir: &Path) {
		self.current_dir(dir);
	}

	fn stdin(&mut self, stdio: Stdio) {
		self.stdin(stdio);
	}

	fn stdout(&mut self, stdio: Stdio) {
		self.stdout(stdio);
	}

	fn stderr(&mut self, stdio: Stdio) {
		self.stderr(stdio);
	}

	fn get_program(&self) -> &OsStr {
		self.as_std().get_program()
	}

	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		Box::new(self.as_std().get_args())
	}

	fn get_current_dir(&self) -> Option<&Path> {
		self.as_std().get_current_dir()
	}
}

#[derive(Clone, Debug)]
pub(crate) enum CommandArg {
	Regular(OsString),
	#[cfg(windows)]
	Raw(OsString),
}

impl CommandArg {
	fn value(&self) -> &OsStr {
		match self {
			Self::Regular(value) => value,
			#[cfg(windows)]
			Self::Raw(value) => value,
		}
	}
}

#[derive(Clone, Debug)]
pub(crate) enum EnvChange {
	Set(OsString, OsString),
	Remove(OsString),
}

#[derive(Clone, Debug)]
pub(crate) struct CommandIntent {
	pub(crate) program: OsString,
	pub(crate) args: Vec<CommandArg>,
	pub(crate) env_clear: bool,
	pub(crate) env: Vec<EnvChange>,
	pub(crate) current_dir: Option<PathBuf>,
}

impl CommandIntent {
	fn new(program: impl AsRef<OsStr>) -> Self {
		Self {
			program: program.as_ref().to_owned(),
			args: Vec::new(),
			env_clear: false,
			env: Vec::new(),
			current_dir: None,
		}
	}

	pub(crate) fn materialize<N: NativeCommand>(&self) -> N {
		let mut command = N::new(&self.program);

		for arg in &self.args {
			match arg {
				CommandArg::Regular(arg) => command.arg(arg),
				#[cfg(windows)]
				CommandArg::Raw(arg) => command.raw_arg(arg),
			}
		}

		if self.env_clear {
			command.env_clear();
		}
		for change in &self.env {
			match change {
				EnvChange::Set(key, value) => command.env(key, value),
				EnvChange::Remove(key) => command.env_remove(key),
			}
		}

		if let Some(dir) = &self.current_dir {
			command.current_dir(dir);
		}

		command
	}
}

enum CommandState<N> {
	Tracked(CommandIntent),
	NativeOnly(N),
	Transitioning,
}

impl<N: fmt::Debug> fmt::Debug for CommandState<N> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Tracked(intent) => f.debug_tuple("Tracked").field(intent).finish(),
			Self::NativeOnly(command) => f.debug_tuple("NativeOnly").field(command).finish(),
			Self::Transitioning => f.write_str("Transitioning"),
		}
	}
}

/// A configurable process command with composable wrappers.
///
/// The backend type is normally selected through `process_wrap::std::Command` or
/// `process_wrap::tokio::Command`. Command construction and configuration are shared; spawning and
/// child behavior remain specific to the selected frontend.
pub struct Command<B: Backend> {
	state: CommandState<B::NativeCommand>,
	wrappers: Box<dyn Any + Send + Sync>,
	backend: PhantomData<fn() -> B>,
}

impl<B: Backend> fmt::Debug for Command<B> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Command")
			.field("state", &self.state)
			.finish_non_exhaustive()
	}
}

impl<B: Backend> Command<B> {
	/// Create a command for `program`.
	pub fn new(program: impl AsRef<OsStr>) -> Self {
		Self {
			state: CommandState::Tracked(CommandIntent::new(program)),
			wrappers: B::new_registry(),
			backend: PhantomData,
		}
	}

	/// Create a command and configure it with a closure.
	pub fn with_new(program: impl AsRef<OsStr>, init: impl FnOnce(&mut Self)) -> Self {
		let mut command = Self::new(program);
		init(&mut command);
		command
	}

	/// Get a compatibility view of this process-wrap command.
	pub fn command(&self) -> &Self {
		self
	}

	/// Get a mutable compatibility view of this process-wrap command.
	pub fn command_mut(&mut self) -> &mut Self {
		self
	}

	/// Discard all wrappers and return this process-wrap command.
	pub fn into_command(mut self) -> Self {
		self.wrappers = B::new_registry();
		self
	}

	/// Add an argument.
	pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		let arg = arg.as_ref();
		match &mut self.state {
			CommandState::Tracked(intent) => intent.args.push(CommandArg::Regular(arg.to_owned())),
			CommandState::NativeOnly(command) => command.arg(arg),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
		self
	}

	/// Add multiple arguments.
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

	/// Add a raw command-line fragment without quoting or escaping.
	///
	/// This method is only available on Windows.
	#[cfg(windows)]
	pub fn raw_arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		let arg = arg.as_ref();
		match &mut self.state {
			CommandState::Tracked(intent) => intent.args.push(CommandArg::Raw(arg.to_owned())),
			CommandState::NativeOnly(command) => command.raw_arg(arg),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
		self
	}

	/// Set an environment variable.
	pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
		let key = key.as_ref();
		let value = value.as_ref();
		match &mut self.state {
			CommandState::Tracked(intent) => intent
				.env
				.push(EnvChange::Set(key.to_owned(), value.to_owned())),
			CommandState::NativeOnly(command) => command.env(key, value),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
		self
	}

	/// Set multiple environment variables.
	pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
	where
		I: IntoIterator<Item = (K, V)>,
		K: AsRef<OsStr>,
		V: AsRef<OsStr>,
	{
		for (key, value) in vars {
			self.env(key, value);
		}
		self
	}

	/// Remove an environment variable from the child environment.
	pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
		let key = key.as_ref();
		match &mut self.state {
			CommandState::Tracked(intent) => intent.env.push(EnvChange::Remove(key.to_owned())),
			CommandState::NativeOnly(command) => command.env_remove(key),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
		self
	}

	/// Clear explicitly configured variables and prevent inheriting the parent environment.
	pub fn env_clear(&mut self) -> &mut Self {
		match &mut self.state {
			CommandState::Tracked(intent) => {
				intent.env_clear = true;
				intent.env.clear();
			}
			CommandState::NativeOnly(command) => command.env_clear(),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
		self
	}

	/// Set the child process's current directory.
	pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
		let dir = dir.as_ref();
		match &mut self.state {
			CommandState::Tracked(intent) => intent.current_dir = Some(dir.to_owned()),
			CommandState::NativeOnly(command) => command.current_dir(dir),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
		self
	}

	/// Configure standard input and make the command native-only.
	pub fn stdin(&mut self, stdio: Stdio) -> &mut Self {
		self.native_mut().stdin(stdio);
		self
	}

	/// Configure standard output and make the command native-only.
	pub fn stdout(&mut self, stdio: Stdio) -> &mut Self {
		self.native_mut().stdout(stdio);
		self
	}

	/// Configure standard error and make the command native-only.
	pub fn stderr(&mut self, stdio: Stdio) -> &mut Self {
		self.native_mut().stderr(stdio);
		self
	}

	/// Get the configured program.
	pub fn get_program(&self) -> &OsStr {
		match &self.state {
			CommandState::Tracked(intent) => &intent.program,
			CommandState::NativeOnly(command) => command.get_program(),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
	}

	/// Get the configured arguments.
	pub fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		match &self.state {
			CommandState::Tracked(intent) => Box::new(intent.args.iter().map(CommandArg::value)),
			CommandState::NativeOnly(command) => command.get_args(),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
	}

	/// Get the configured current directory.
	pub fn get_current_dir(&self) -> Option<&Path> {
		match &self.state {
			CommandState::Tracked(intent) => intent.current_dir.as_deref(),
			CommandState::NativeOnly(command) => command.get_current_dir(),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
	}

	/// Mutably access the frontend's native command.
	///
	/// Calling this permanently makes the command native-only. Alternate portable transports cannot
	/// recover exact portable intent after arbitrary native mutation.
	pub fn native_mut(&mut self) -> &mut B::NativeCommand {
		if matches!(self.state, CommandState::Tracked(_)) {
			let state = std::mem::replace(&mut self.state, CommandState::Transitioning);
			self.state = match state {
				CommandState::Tracked(intent) => {
					CommandState::NativeOnly(intent.materialize::<B::NativeCommand>())
				}
				_ => unreachable!("tracked command state was checked before transition"),
			};
		}

		match &mut self.state {
			CommandState::NativeOnly(command) => command,
			CommandState::Tracked(_) => unreachable!("tracked command was materialized above"),
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
	}

	/// Consume this command and return the frontend's native command.
	pub fn into_native(self) -> B::NativeCommand {
		match self.state {
			CommandState::Tracked(intent) => intent.materialize::<B::NativeCommand>(),
			CommandState::NativeOnly(command) => command,
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
	}

	pub(crate) fn from_native(command: B::NativeCommand) -> Self {
		Self {
			state: CommandState::NativeOnly(command),
			wrappers: B::new_registry(),
			backend: PhantomData,
		}
	}

	pub(crate) fn registry<R: Any>(&self) -> &R {
		self.wrappers
			.downcast_ref()
			.expect("the backend always creates its matching wrapper registry")
	}

	pub(crate) fn registry_mut<R: Any>(&mut self) -> &mut R {
		self.wrappers
			.downcast_mut()
			.expect("the backend always creates its matching wrapper registry")
	}

	pub(crate) fn with_native<T>(
		&mut self,
		invoke: impl FnOnce(&mut Self, &mut B::NativeCommand) -> std::io::Result<T>,
	) -> std::io::Result<T> {
		match &self.state {
			CommandState::Tracked(intent) => {
				let mut native = intent.materialize::<B::NativeCommand>();
				invoke(self, &mut native)
			}
			CommandState::NativeOnly(_) => {
				let state = std::mem::replace(&mut self.state, CommandState::Transitioning);
				let mut native = match state {
					CommandState::NativeOnly(native) => native,
					_ => unreachable!("native-only command state was checked before transition"),
				};
				let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
					invoke(self, &mut native)
				}));
				self.state = CommandState::NativeOnly(native);
				match result {
					Ok(result) => result,
					Err(payload) => std::panic::resume_unwind(payload),
				}
			}
			CommandState::Transitioning => {
				unreachable!("command state is restored before returning")
			}
		}
	}
}
