#![cfg_attr(not(any(feature = "std", feature = "tokio1")), allow(dead_code))]

use std::{
	any::Any,
	ffi::{OsStr, OsString},
	fmt,
	marker::PhantomData,
	path::{Path, PathBuf},
	process::Stdio,
};

#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle};

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

	/// Register a callback to run in the child after `fork`.
	#[cfg(unix)]
	unsafe fn pre_exec<F>(&mut self, callback: F)
	where
		F: FnMut() -> std::io::Result<()> + Send + Sync + 'static;

	/// Configure whether dropping a child kills it, where the frontend supports that policy.
	fn configure_kill_on_drop(&mut self, kill_on_drop: bool) {
		debug_assert!(!kill_on_drop, "only Tokio commands support kill-on-drop");
	}

	/// Set Windows process creation flags.
	#[cfg(windows)]
	fn creation_flags(&mut self, flags: u32);

	/// Get the program.
	fn get_program(&self) -> &OsStr;

	/// Get the arguments.
	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_>;

	/// Get explicitly configured environment changes.
	fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_>;

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

	#[cfg(unix)]
	unsafe fn pre_exec<F>(&mut self, callback: F)
	where
		F: FnMut() -> std::io::Result<()> + Send + Sync + 'static,
	{
		use std::os::unix::process::CommandExt;
		// SAFETY: the caller accepts the native `pre_exec` contract.
		unsafe { CommandExt::pre_exec(self, callback) };
	}

	#[cfg(windows)]
	fn creation_flags(&mut self, flags: u32) {
		use std::os::windows::process::CommandExt;
		CommandExt::creation_flags(self, flags);
	}

	fn get_program(&self) -> &OsStr {
		self.get_program()
	}

	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		Box::new(self.get_args())
	}

	fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_> {
		Box::new(self.get_envs())
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

	#[cfg(unix)]
	unsafe fn pre_exec<F>(&mut self, callback: F)
	where
		F: FnMut() -> std::io::Result<()> + Send + Sync + 'static,
	{
		// SAFETY: the caller accepts the native `pre_exec` contract.
		unsafe { self.pre_exec(callback) };
	}

	fn configure_kill_on_drop(&mut self, kill_on_drop: bool) {
		self.kill_on_drop(kill_on_drop);
	}

	#[cfg(windows)]
	fn creation_flags(&mut self, flags: u32) {
		self.creation_flags(flags);
	}

	fn get_program(&self) -> &OsStr {
		self.as_std().get_program()
	}

	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		Box::new(self.as_std().get_args())
	}

	fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_> {
		Box::new(self.as_std().get_envs())
	}

	fn get_current_dir(&self) -> Option<&Path> {
		self.as_std().get_current_dir()
	}
}

/// One losslessly tracked command-line argument.
///
/// [`Command::get_args`] and [`SpawnAttempt::get_args`] provide native-shaped value iterators. This
/// type additionally preserves whether a Windows argument was supplied through `raw_arg`, which an
/// alternate spawn provider needs in order to reproduce or reject the exact command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandArg {
	/// A regular argument which the transport must quote according to its command-line model.
	Regular(OsString),
	/// A raw Windows command-line fragment which the transport must not quote or escape.
	#[cfg(windows)]
	#[cfg_attr(docsrs, doc(cfg(windows)))]
	Raw(OsString),
}

impl CommandArg {
	/// Return the argument or raw fragment value.
	pub fn value(&self) -> &OsStr {
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

impl EnvChange {
	fn key(&self) -> &OsStr {
		match self {
			Self::Set(key, _) | Self::Remove(key) => key,
		}
	}

	fn value(&self) -> Option<&OsStr> {
		match self {
			Self::Set(_, value) => Some(value),
			Self::Remove(_) => None,
		}
	}
}

#[cfg(not(windows))]
fn env_keys_equal(left: &OsStr, right: &OsStr) -> bool {
	left == right
}

#[cfg(windows)]
fn env_keys_equal(left: &OsStr, right: &OsStr) -> bool {
	use std::os::windows::ffi::OsStrExt;

	#[link(name = "kernel32")]
	unsafe extern "system" {
		#[link_name = "CompareStringOrdinal"]
		fn compare_string_ordinal(
			string1: *const u16,
			count1: i32,
			string2: *const u16,
			count2: i32,
			ignore_case: i32,
		) -> i32;
	}

	let left = left.encode_wide().collect::<Vec<_>>();
	let right = right.encode_wide().collect::<Vec<_>>();
	let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
	else {
		return false;
	};

	// SAFETY: both pointers remain valid for their explicit lengths during the call. The API does not
	// require NUL termination when lengths are supplied.
	unsafe { compare_string_ordinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == 2 }
}

struct EnvChanges<'a> {
	changes: &'a [EnvChange],
	index: usize,
}

impl<'a> Iterator for EnvChanges<'a> {
	type Item = (&'a OsStr, Option<&'a OsStr>);

	fn next(&mut self) -> Option<Self::Item> {
		while let Some(change) = self.changes.get(self.index) {
			self.index += 1;
			if self.changes[self.index..]
				.iter()
				.any(|later| env_keys_equal(change.key(), later.key()))
			{
				continue;
			}

			return Some((change.key(), change.value()));
		}

		None
	}
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

	fn env_remove(&mut self, key: &OsStr) {
		if self.env_clear {
			self.env.retain(|change| !env_keys_equal(change.key(), key));
		} else {
			self.env.push(EnvChange::Remove(key.to_owned()));
		}
	}

	fn get_envs(&self) -> EnvChanges<'_> {
		EnvChanges {
			changes: &self.env,
			index: 0,
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

#[derive(Debug)]
struct NativeCommandView {
	program: OsString,
	args: Vec<OsString>,
	env: Vec<(OsString, Option<OsString>)>,
	current_dir: Option<PathBuf>,
}

impl NativeCommandView {
	fn capture<N: NativeCommand>(command: &N) -> Self {
		Self {
			program: command.get_program().to_owned(),
			args: command.get_args().map(OsStr::to_owned).collect(),
			env: command
				.get_envs()
				.map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
				.collect(),
			current_dir: command.get_current_dir().map(Path::to_owned),
		}
	}

	fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		Box::new(self.args.iter().map(OsString::as_os_str))
	}

	fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_> {
		Box::new(
			self.env
				.iter()
				.map(|(key, value)| (key.as_os_str(), value.as_deref())),
		)
	}
}

struct NativeOnlyCommand<N> {
	command: Option<N>,
	view: NativeCommandView,
}

impl<N: NativeCommand> NativeOnlyCommand<N> {
	fn new(command: N) -> Self {
		Self {
			view: NativeCommandView::capture(&command),
			command: Some(command),
		}
	}

	fn command_mut(&mut self) -> &mut N {
		self.command
			.as_mut()
			.expect("native command access cannot occur while a spawn lifecycle is active")
	}

	fn take(&mut self) -> N {
		let command = self
			.command
			.take()
			.expect("a native-only command is present when its spawn lifecycle begins");
		self.view = NativeCommandView::capture(&command);
		command
	}

	fn restore(&mut self, command: N) {
		debug_assert!(self.command.is_none());
		self.command = Some(command);
	}

	fn into_command(self) -> N {
		self.command
			.expect("a command cannot be consumed while its spawn lifecycle is active")
	}
}

impl<N: fmt::Debug> fmt::Debug for NativeOnlyCommand<N> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("NativeOnly")
			.field("command", &self.command)
			.field("view", &self.view)
			.finish()
	}
}

enum CommandState<N> {
	Tracked(CommandIntent),
	NativeOnly(NativeOnlyCommand<N>),
}

impl<N: fmt::Debug> fmt::Debug for CommandState<N> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Tracked(intent) => f.debug_tuple("Tracked").field(intent).finish(),
			Self::NativeOnly(command) => command.fmt(f),
		}
	}
}

/// Cleanup and finalization owned by an alternate spawn provider.
///
/// A provider returns a fresh, armed transaction with every child it successfully creates. The
/// transaction must own its cleanup resources independently of the child wrapper chain, because a
/// failing child wrapper may already have consumed or dropped that chain.
///
/// Process-wrap calls [`commit`](SpawnTransaction::commit) only after every public post-spawn and
/// child-wrapping hook succeeds. `commit` must disarm rollback resources on success. If it returns an
/// error or panics, the transaction must remain rollbackable; process-wrap then makes one best-effort
/// [`rollback`](SpawnTransaction::rollback) call. A rollback error or panic is suppressed so the
/// original error or panic is preserved. After a successful commit, process-wrap drops the transaction
/// and does not roll it back if a later internal child-finalization phase fails.
///
/// Until a provider returns its `ProviderProduct`, cleanup for errors or panics in its own `spawn`
/// implementation remains the provider's responsibility.
pub trait SpawnTransaction: fmt::Debug + Send + 'static {
	/// Finalize the successful spawn and disarm rollback resources.
	fn commit(&mut self) -> std::io::Result<()>;

	/// Undo an uncommitted spawn.
	fn rollback(&mut self) -> std::io::Result<()>;
}

#[cfg(windows)]
const CREATE_SUSPENDED_FLAG: u32 = 0x0000_0004;

/// Portable Windows process-creation policy for one spawn attempt.
///
/// Alternate providers use this policy to preserve creation flags and compose with `JobObject` and
/// Tokio `KillOnDrop` without inspecting an opaque native command.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WindowsSpawnPolicy {
	user_creation_flags: u32,
	spawn_creation_flags: u32,
	has_creation_flags: bool,
	has_job_object: bool,
	kill_on_drop: bool,
}

#[cfg(windows)]
impl WindowsSpawnPolicy {
	/// Return the flags explicitly requested through `CreationFlags`.
	pub fn user_creation_flags(self) -> u32 {
		self.user_creation_flags
	}

	/// Return the complete flags the transport must use when creating the process.
	///
	/// This includes process-wrap's temporary `CREATE_SUSPENDED` flag when a job object must be
	/// assigned before the process starts running.
	pub fn spawn_creation_flags(self) -> u32 {
		self.spawn_creation_flags
	}

	/// Return whether `CreationFlags` configured this attempt.
	pub fn has_creation_flags(self) -> bool {
		self.has_creation_flags
	}

	/// Return whether this attempt must assign the child to a job object.
	pub fn has_job_object(self) -> bool {
		self.has_job_object
	}

	/// Return whether the caller explicitly requested `CREATE_SUSPENDED`.
	pub fn is_explicitly_suspended(self) -> bool {
		self.user_creation_flags & CREATE_SUSPENDED_FLAG != 0
	}

	/// Return whether process-wrap added temporary suspension for job-object assignment.
	pub fn is_temporarily_suspended(self) -> bool {
		self.has_job_object && !self.is_explicitly_suspended()
	}

	/// Return whether the child starts suspended for either reason.
	pub fn starts_suspended(self) -> bool {
		self.spawn_creation_flags & CREATE_SUSPENDED_FLAG != 0
	}

	/// Return whether dropping the direct Tokio child must terminate it.
	pub fn kills_on_drop(self) -> bool {
		self.kill_on_drop
	}

	#[cfg(any(feature = "creation-flags", test))]
	fn set_creation_flags(&mut self, flags: u32) {
		self.user_creation_flags = flags;
		self.has_creation_flags = true;
		self.recompute_spawn_flags();
	}

	#[cfg(any(feature = "job-object", test))]
	fn set_job_object(&mut self) {
		self.has_job_object = true;
		self.recompute_spawn_flags();
	}

	#[cfg(any(all(feature = "tokio1", feature = "kill-on-drop"), test))]
	fn set_kill_on_drop(&mut self, kill_on_drop: bool) {
		self.kill_on_drop = kill_on_drop;
	}

	#[cfg(any(feature = "creation-flags", feature = "job-object", test))]
	fn recompute_spawn_flags(&mut self) {
		self.spawn_creation_flags = self.user_creation_flags;
		if self.has_job_object {
			self.spawn_creation_flags |= CREATE_SUSPENDED_FLAG;
		}
	}

	fn applies_creation_flags(self) -> bool {
		self.has_creation_flags || self.has_job_object
	}
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
	fn TerminateProcess(process: *mut std::ffi::c_void, exit_code: u32) -> i32;
	fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
}

#[cfg(windows)]
const WAIT_FAILED: u32 = u32::MAX;
#[cfg(windows)]
const WAIT_INFINITE: u32 = u32::MAX;

#[cfg(windows)]
pub(crate) fn terminate_process_and_wait(process: BorrowedHandle<'_>) -> std::io::Result<()> {
	let raw = process.as_raw_handle();
	// SAFETY: `raw` is a live process handle for both calls and remains borrowed until they finish.
	if unsafe { TerminateProcess(raw, 1) } == 0 {
		return Err(std::io::Error::last_os_error());
	}
	// SAFETY: the process handle remains live for the duration of this call.
	if unsafe { WaitForSingleObject(raw, WAIT_INFINITE) } == WAIT_FAILED {
		Err(std::io::Error::last_os_error())
	} else {
		Ok(())
	}
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct WindowsSpawnCleanup {
	process: OwnedHandle,
	armed: bool,
}

#[cfg(windows)]
impl WindowsSpawnCleanup {
	pub(crate) fn new(process: BorrowedHandle<'_>) -> std::io::Result<Self> {
		Ok(Self {
			process: process.try_clone_to_owned()?,
			armed: true,
		})
	}

	pub(crate) fn disarm(&mut self) {
		self.armed = false;
	}
}

#[cfg(windows)]
impl Drop for WindowsSpawnCleanup {
	fn drop(&mut self) {
		if self.armed {
			let _ = terminate_process_and_wait(self.process.as_handle());
		}
	}
}

enum AttemptState<N> {
	Tracked {
		intent: CommandIntent,
		native: Option<N>,
	},
	NativeOnly(N),
}

impl<N: fmt::Debug> fmt::Debug for AttemptState<N> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Tracked { intent, native } => f
				.debug_struct("Tracked")
				.field("intent", intent)
				.field("native", native)
				.finish(),
			Self::NativeOnly(command) => f.debug_tuple("NativeOnly").field(command).finish(),
		}
	}
}

#[derive(Clone, Debug, Default)]
struct PlatformCommandState {
	#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
	unix: crate::unix::CommandState,
}

/// The command configuration for one spawn attempt.
///
/// Each call to a spawn method creates a fresh attempt. Hooks may modify an attempt copied from a
/// tracked base [`Command`] without changing that base. A native-only command instead lends its exact
/// native command to the attempt and retains hook mutations when the command is restored afterward.
/// Explicit native mutation makes a tracked attempt native-only, which alternate portable spawn
/// providers reject rather than reconstructing or partially applying.
pub struct SpawnAttempt<B: Backend> {
	state: AttemptState<B::NativeCommand>,
	#[cfg_attr(not(unix), allow(dead_code))]
	platform: PlatformCommandState,
	#[cfg_attr(not(unix), allow(dead_code))]
	native_only_base: bool,
	kill_on_drop: Option<bool>,
	#[cfg(windows)]
	windows_policy: WindowsSpawnPolicy,
	#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
	unix_policy: crate::unix::SpawnPolicy,
	backend: PhantomData<fn() -> B>,
}

impl<B: Backend> fmt::Debug for SpawnAttempt<B> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("SpawnAttempt")
			.field("state", &self.state)
			.finish()
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
	platform: PlatformCommandState,
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
			platform: PlatformCommandState::default(),
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
			CommandState::NativeOnly(command) => command.command_mut().arg(arg),
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
			CommandState::NativeOnly(command) => command.command_mut().raw_arg(arg),
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
			CommandState::NativeOnly(command) => command.command_mut().env(key, value),
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
			CommandState::Tracked(intent) => intent.env_remove(key),
			CommandState::NativeOnly(command) => command.command_mut().env_remove(key),
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
			CommandState::NativeOnly(command) => command.command_mut().env_clear(),
		}
		self
	}

	/// Set the child process's current directory.
	pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
		let dir = dir.as_ref();
		match &mut self.state {
			CommandState::Tracked(intent) => intent.current_dir = Some(dir.to_owned()),
			CommandState::NativeOnly(command) => command.command_mut().current_dir(dir),
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
			CommandState::NativeOnly(command) => match &command.command {
				Some(command) => command.get_program(),
				None => &command.view.program,
			},
		}
	}

	/// Get the configured arguments.
	pub fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		match &self.state {
			CommandState::Tracked(intent) => Box::new(intent.args.iter().map(CommandArg::value)),
			CommandState::NativeOnly(command) => match &command.command {
				Some(command) => command.get_args(),
				None => command.view.get_args(),
			},
		}
	}

	/// Get the losslessly tracked portable arguments in command-line order.
	///
	/// Unlike [`get_args`](Self::get_args), this preserves regular versus raw Windows arguments. Returns
	/// `None` for a native-only command because its native API cannot recover that distinction.
	pub fn get_portable_args(&self) -> Option<&[CommandArg]> {
		match &self.state {
			CommandState::Tracked(intent) => Some(&intent.args),
			CommandState::NativeOnly(_) => None,
		}
	}

	/// Get explicitly configured environment changes.
	pub fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_> {
		match &self.state {
			CommandState::Tracked(intent) => Box::new(intent.get_envs()),
			CommandState::NativeOnly(command) => match &command.command {
				Some(command) => command.get_envs(),
				None => command.view.get_envs(),
			},
		}
	}

	/// Return whether this portable command inherits the parent environment.
	///
	/// Returns `Some(true)` for normal inheritance, `Some(false)` after `env_clear`, and `None` for a
	/// native-only command because native command APIs do not expose that state.
	pub fn inherits_environment(&self) -> Option<bool> {
		match &self.state {
			CommandState::Tracked(intent) => Some(!intent.env_clear),
			CommandState::NativeOnly(_) => None,
		}
	}

	/// Get the configured current directory.
	pub fn get_current_dir(&self) -> Option<&Path> {
		match &self.state {
			CommandState::Tracked(intent) => intent.current_dir.as_deref(),
			CommandState::NativeOnly(command) => match &command.command {
				Some(command) => command.get_current_dir(),
				None => command.view.current_dir.as_deref(),
			},
		}
	}

	/// Mutably access the frontend's native command.
	///
	/// Calling this permanently makes the command native-only. Alternate portable transports cannot
	/// recover exact portable intent after arbitrary native mutation. On Unix, process-wrap reinstalls
	/// any built-in child setup when it next spawns the command, so replacing the native value does not
	/// discard that setup.
	pub fn native_mut(&mut self) -> &mut B::NativeCommand {
		if let CommandState::Tracked(intent) = &self.state {
			let command = intent.materialize::<B::NativeCommand>();
			self.state = CommandState::NativeOnly(NativeOnlyCommand::new(command));
		}

		#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
		self.platform.unix.invalidate();

		match &mut self.state {
			CommandState::NativeOnly(command) => command.command_mut(),
			CommandState::Tracked(_) => unreachable!("tracked command was materialized above"),
		}
	}

	/// Consume this command and return the frontend's native command.
	pub fn into_native(self) -> B::NativeCommand {
		match self.state {
			CommandState::Tracked(intent) => intent.materialize::<B::NativeCommand>(),
			CommandState::NativeOnly(command) => command.into_command(),
		}
	}

	pub(crate) fn from_native(command: B::NativeCommand) -> Self {
		Self {
			state: CommandState::NativeOnly(NativeOnlyCommand::new(command)),
			wrappers: B::new_registry(),
			platform: PlatformCommandState::default(),
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

	/// Return whether this command contains opaque native-only state.
	pub fn is_native_only(&self) -> bool {
		matches!(self.state, CommandState::NativeOnly(_))
	}

	pub(crate) fn with_spawn_attempt<T>(
		&mut self,
		invoke: impl FnOnce(&mut Self, &mut SpawnAttempt<B>) -> std::io::Result<T>,
	) -> std::io::Result<T> {
		let platform = self.platform.clone();
		match &mut self.state {
			CommandState::Tracked(intent) => {
				let mut attempt = SpawnAttempt {
					state: AttemptState::Tracked {
						intent: intent.clone(),
						native: None,
					},
					platform,
					native_only_base: false,
					kill_on_drop: None,
					#[cfg(windows)]
					windows_policy: WindowsSpawnPolicy::default(),
					#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
					unix_policy: crate::unix::SpawnPolicy::default(),
					backend: PhantomData,
				};
				let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
					invoke(self, &mut attempt)
				}));
				attempt.disarm_platform();
				match result {
					Ok(result) => result,
					Err(payload) => std::panic::resume_unwind(payload),
				}
			}
			CommandState::NativeOnly(command) => {
				let native = command.take();
				let mut attempt = SpawnAttempt {
					state: AttemptState::NativeOnly(native),
					platform,
					native_only_base: true,
					kill_on_drop: None,
					#[cfg(windows)]
					windows_policy: WindowsSpawnPolicy::default(),
					#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
					unix_policy: crate::unix::SpawnPolicy::default(),
					backend: PhantomData,
				};
				let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
					invoke(self, &mut attempt)
				}));
				attempt.disarm_platform();
				let native = match attempt.state {
					AttemptState::NativeOnly(native) => native,
					AttemptState::Tracked { .. } => {
						unreachable!("a native-only spawn attempt cannot become tracked")
					}
				};
				match &mut self.state {
					CommandState::NativeOnly(command) => command.restore(native),
					CommandState::Tracked(_) => {
						unreachable!("a spawn lifecycle cannot replace native-only command state")
					}
				}
				match result {
					Ok(result) => result,
					Err(payload) => std::panic::resume_unwind(payload),
				}
			}
		}
	}
}

impl<B: Backend> SpawnAttempt<B> {
	/// Add an argument for this spawn attempt.
	pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		let arg = arg.as_ref();
		match &mut self.state {
			AttemptState::Tracked { intent, .. } => {
				intent.args.push(CommandArg::Regular(arg.to_owned()))
			}
			AttemptState::NativeOnly(command) => command.arg(arg),
		}
		self
	}

	/// Add multiple arguments for this spawn attempt.
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
			AttemptState::Tracked { intent, .. } => {
				intent.args.push(CommandArg::Raw(arg.to_owned()))
			}
			AttemptState::NativeOnly(command) => command.raw_arg(arg),
		}
		self
	}

	/// Set an environment variable for this spawn attempt.
	pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
		let key = key.as_ref();
		let value = value.as_ref();
		match &mut self.state {
			AttemptState::Tracked { intent, .. } => intent
				.env
				.push(EnvChange::Set(key.to_owned(), value.to_owned())),
			AttemptState::NativeOnly(command) => command.env(key, value),
		}
		self
	}

	/// Set multiple environment variables for this spawn attempt.
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

	/// Remove an environment variable for this spawn attempt.
	pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
		let key = key.as_ref();
		match &mut self.state {
			AttemptState::Tracked { intent, .. } => intent.env_remove(key),
			AttemptState::NativeOnly(command) => command.env_remove(key),
		}
		self
	}

	/// Clear configured variables and prevent inheritance for this spawn attempt.
	pub fn env_clear(&mut self) -> &mut Self {
		match &mut self.state {
			AttemptState::Tracked { intent, .. } => {
				intent.env_clear = true;
				intent.env.clear();
			}
			AttemptState::NativeOnly(command) => command.env_clear(),
		}
		self
	}

	/// Set the child process's current directory for this spawn attempt.
	pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
		let dir = dir.as_ref();
		match &mut self.state {
			AttemptState::Tracked { intent, .. } => intent.current_dir = Some(dir.to_owned()),
			AttemptState::NativeOnly(command) => command.current_dir(dir),
		}
		self
	}

	/// Configure standard input and make this spawn attempt native-only.
	pub fn stdin(&mut self, stdio: Stdio) -> &mut Self {
		self.native_mut().stdin(stdio);
		self
	}

	/// Configure standard output and make this spawn attempt native-only.
	pub fn stdout(&mut self, stdio: Stdio) -> &mut Self {
		self.native_mut().stdout(stdio);
		self
	}

	/// Configure standard error and make this spawn attempt native-only.
	pub fn stderr(&mut self, stdio: Stdio) -> &mut Self {
		self.native_mut().stderr(stdio);
		self
	}

	/// Get the configured program for this spawn attempt.
	pub fn get_program(&self) -> &OsStr {
		match &self.state {
			AttemptState::Tracked { intent, .. } => &intent.program,
			AttemptState::NativeOnly(command) => command.get_program(),
		}
	}

	/// Get the configured arguments for this spawn attempt.
	pub fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
		match &self.state {
			AttemptState::Tracked { intent, .. } => {
				Box::new(intent.args.iter().map(CommandArg::value))
			}
			AttemptState::NativeOnly(command) => command.get_args(),
		}
	}

	/// Get the losslessly tracked portable arguments in command-line order.
	///
	/// Unlike [`get_args`](Self::get_args), this preserves regular versus raw Windows arguments. Returns
	/// `None` for a native-only attempt. Process-wrap performs that rejection before a provider's
	/// `validate_attempt` callback, so providers receive `Some` there.
	pub fn get_portable_args(&self) -> Option<&[CommandArg]> {
		match &self.state {
			AttemptState::Tracked { intent, .. } => Some(&intent.args),
			AttemptState::NativeOnly(_) => None,
		}
	}

	/// Get explicitly configured environment changes for this spawn attempt.
	pub fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_> {
		match &self.state {
			AttemptState::Tracked { intent, .. } => Box::new(intent.get_envs()),
			AttemptState::NativeOnly(command) => command.get_envs(),
		}
	}

	/// Return whether this portable attempt inherits the parent environment.
	///
	/// Returns `Some(true)` for normal inheritance, `Some(false)` after `env_clear`, and `None` for a
	/// native-only attempt. Process-wrap performs that rejection before a provider's `validate_attempt`
	/// callback, so providers receive `Some` there.
	pub fn inherits_environment(&self) -> Option<bool> {
		match &self.state {
			AttemptState::Tracked { intent, .. } => Some(!intent.env_clear),
			AttemptState::NativeOnly(_) => None,
		}
	}

	/// Get the configured current directory for this spawn attempt.
	pub fn get_current_dir(&self) -> Option<&Path> {
		match &self.state {
			AttemptState::Tracked { intent, .. } => intent.current_dir.as_deref(),
			AttemptState::NativeOnly(command) => command.get_current_dir(),
		}
	}

	/// Return whether dropping the direct child must terminate it.
	///
	/// This is currently a Tokio policy. Alternate Tokio providers use it to preserve the behavior of
	/// the `KillOnDrop` wrapper without requiring a native Tokio command.
	pub fn kills_on_drop(&self) -> bool {
		self.kill_on_drop.unwrap_or(false)
	}

	/// Return the portable Windows creation policy for this attempt.
	#[cfg(windows)]
	pub fn windows_spawn_policy(&self) -> WindowsSpawnPolicy {
		self.windows_policy
	}

	#[cfg(all(feature = "tokio1", feature = "kill-on-drop"))]
	pub(crate) fn set_kill_on_drop(&mut self, kill_on_drop: bool) {
		self.kill_on_drop = Some(kill_on_drop);
		#[cfg(windows)]
		self.windows_policy.set_kill_on_drop(kill_on_drop);
	}

	#[cfg(all(windows, feature = "creation-flags"))]
	pub(crate) fn set_windows_creation_flags(&mut self, flags: u32) {
		self.windows_policy.set_creation_flags(flags);
	}

	#[cfg(all(windows, feature = "job-object"))]
	pub(crate) fn set_job_object(&mut self) {
		self.windows_policy.set_job_object();
	}

	#[cfg(windows)]
	pub(crate) fn starts_suspended(&self) -> bool {
		self.windows_policy.starts_suspended()
	}

	/// Return the process-group setup requested for this attempt.
	///
	/// Alternate spawn providers use this to apply process-group intent without making the attempt
	/// native-only. This method remains available when the `process-group` wrapper feature is disabled
	/// and returns `None` in that configuration.
	#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
	pub fn process_group_target(&self) -> Option<crate::ProcessGroupTarget> {
		self.unix_policy.process_group
	}

	/// Return whether this attempt must create a new process session.
	///
	/// This method remains available when the `process-session` wrapper feature is disabled and returns
	/// `false` in that configuration.
	#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
	pub fn creates_process_session(&self) -> bool {
		self.unix_policy.process_session
	}

	/// Return whether this attempt must reset the child signal mask.
	///
	/// This method remains available when the `reset-sigmask` wrapper feature is disabled and returns
	/// `false` in that configuration.
	#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
	pub fn resets_sigmask(&self) -> bool {
		self.unix_policy.reset_sigmask
	}

	#[cfg(all(
		unix,
		any(feature = "std", feature = "tokio1"),
		feature = "process-group"
	))]
	pub(crate) fn set_process_group(
		&mut self,
		target: crate::unix::ProcessGroupTarget,
	) -> std::io::Result<()> {
		self.unix_policy.set_process_group(target)
	}

	#[cfg(all(
		unix,
		any(feature = "std", feature = "tokio1"),
		feature = "process-session"
	))]
	pub(crate) fn set_process_session(&mut self) -> std::io::Result<()> {
		self.unix_policy.set_process_session()
	}

	#[cfg(all(
		unix,
		any(feature = "std", feature = "tokio1"),
		feature = "reset-sigmask"
	))]
	pub(crate) fn set_reset_sigmask(&mut self) {
		self.unix_policy.reset_sigmask = true;
	}

	fn prepare_platform(&mut self) {
		let kill_on_drop = self.kill_on_drop;
		#[cfg(windows)]
		let windows_policy = self.windows_policy;
		{
			let command = match &mut self.state {
				AttemptState::NativeOnly(command) => command,
				AttemptState::Tracked { native, .. } => native
					.as_mut()
					.expect("the attempt is materialized before platform setup"),
			};
			if let Some(kill_on_drop) = kill_on_drop {
				command.configure_kill_on_drop(kill_on_drop);
			}
			#[cfg(windows)]
			if windows_policy.applies_creation_flags() {
				command.creation_flags(windows_policy.spawn_creation_flags());
			}
		}

		#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
		{
			let policy = self.unix_policy;
			let native_only_base = self.native_only_base;
			let command = match &mut self.state {
				AttemptState::NativeOnly(command) => command,
				AttemptState::Tracked { native, .. } => native
					.as_mut()
					.expect("the attempt is materialized before platform setup"),
			};
			self.platform
				.unix
				.prepare(command, native_only_base, policy);
		}
	}

	fn disarm_platform(&mut self) {
		#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
		self.platform.unix.disarm();
	}

	fn materialize_native(&mut self) {
		if let AttemptState::Tracked { intent, native } = &mut self.state {
			if native.is_none() {
				*native = Some(intent.materialize::<B::NativeCommand>());
			}
		}
	}

	fn make_native_only(&mut self) {
		self.materialize_native();
		let native = match &mut self.state {
			AttemptState::Tracked { native, .. } => native
				.take()
				.expect("the tracked attempt was materialized above"),
			AttemptState::NativeOnly(_) => return,
		};
		self.state = AttemptState::NativeOnly(native);
	}

	fn native_command_mut(&mut self) -> &mut B::NativeCommand {
		match &mut self.state {
			AttemptState::Tracked { native, .. } => native
				.as_mut()
				.expect("the tracked attempt was materialized before native access"),
			AttemptState::NativeOnly(command) => command,
		}
	}

	fn invalidate_platform(&mut self) {
		#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
		self.platform.unix.invalidate();
	}

	pub(crate) fn native_for_spawn(&mut self) -> &mut B::NativeCommand {
		self.materialize_native();
		self.prepare_platform();
		self.native_command_mut()
	}

	pub(crate) fn native_for_explicit_spawn(&mut self) -> &mut B::NativeCommand {
		self.make_native_only();
		self.prepare_platform();
		self.native_command_mut()
	}

	/// Mutably access the frontend's native command for this spawn attempt.
	///
	/// Calling this makes only this attempt native-only. An alternate portable provider rejects that
	/// attempt because it cannot recover exact portable intent after arbitrary native mutation. On
	/// Unix, built-in child setup is installed after all pre-spawn hooks have run, so replacing the
	/// native value here does not discard that setup.
	pub fn native_mut(&mut self) -> &mut B::NativeCommand {
		self.make_native_only();
		self.invalidate_platform();
		self.native_command_mut()
	}

	/// Return whether this spawn attempt contains opaque native-only state.
	pub fn is_native_only(&self) -> bool {
		matches!(self.state, AttemptState::NativeOnly(_))
	}
}

#[cfg(all(feature = "std", unix))]
impl SpawnAttempt<Blocking> {
	/// Set the child process's user ID and make this spawn attempt native-only.
	pub fn uid(&mut self, id: u32) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::uid(self.native_mut(), id);
		self
	}

	/// Set the child process's group ID and make this spawn attempt native-only.
	pub fn gid(&mut self, id: u32) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::gid(self.native_mut(), id);
		self
	}

	/// Set the child process's `argv[0]` and make this spawn attempt native-only.
	pub fn arg0(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::arg0(self.native_mut(), arg);
		self
	}

	/// Set the child process's process group and make this spawn attempt native-only.
	pub fn process_group(&mut self, pgroup: i32) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::process_group(self.native_mut(), pgroup);
		self
	}

	/// Register a callback to run in the child after `fork` and make this attempt native-only.
	///
	/// # Safety
	///
	/// The callback runs in the child process after `fork` and before `exec`. It may only perform
	/// operations which are valid in that constrained environment.
	pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Self
	where
		F: FnMut() -> ::std::io::Result<()> + Send + Sync + 'static,
	{
		use ::std::os::unix::process::CommandExt;
		// SAFETY: the caller accepts the native `pre_exec` contract documented above.
		unsafe { CommandExt::pre_exec(self.native_mut(), f) };
		self
	}
}

#[cfg(all(feature = "std", windows))]
impl SpawnAttempt<Blocking> {
	/// Set Windows process creation flags and make this spawn attempt native-only.
	pub fn creation_flags(&mut self, flags: u32) -> &mut Self {
		use ::std::os::windows::process::CommandExt;
		CommandExt::creation_flags(self.native_mut(), flags);
		self
	}
}

#[cfg(feature = "tokio1")]
impl SpawnAttempt<Tokio1> {
	/// Configure whether dropping the Tokio child kills it and make this attempt native-only.
	pub fn kill_on_drop(&mut self, kill_on_drop: bool) -> &mut Self {
		self.native_mut().kill_on_drop(kill_on_drop);
		self
	}
}

#[cfg(all(feature = "tokio1", unix))]
impl SpawnAttempt<Tokio1> {
	/// Set the child process's user ID and make this spawn attempt native-only.
	pub fn uid(&mut self, id: u32) -> &mut Self {
		self.native_mut().uid(id);
		self
	}

	/// Set the child process's group ID and make this spawn attempt native-only.
	pub fn gid(&mut self, id: u32) -> &mut Self {
		self.native_mut().gid(id);
		self
	}

	/// Set the child process's `argv[0]` and make this spawn attempt native-only.
	pub fn arg0(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		self.native_mut().arg0(arg);
		self
	}

	/// Register a callback to run in the child after `fork` and make this attempt native-only.
	///
	/// # Safety
	///
	/// The callback runs in the child process after `fork` and before `exec`. It may only perform
	/// operations which are valid in that constrained environment.
	pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Self
	where
		F: FnMut() -> ::std::io::Result<()> + Send + Sync + 'static,
	{
		// SAFETY: the caller accepts the native `pre_exec` contract documented above.
		unsafe { self.native_mut().pre_exec(f) };
		self
	}
}

#[cfg(all(feature = "tokio1", windows))]
impl SpawnAttempt<Tokio1> {
	/// Set Windows process creation flags and make this spawn attempt native-only.
	pub fn creation_flags(&mut self, flags: u32) -> &mut Self {
		self.native_mut().creation_flags(flags);
		self
	}
}

#[cfg(all(feature = "std", unix))]
impl Command<Blocking> {
	/// Set the child process's user ID and make the command native-only.
	pub fn uid(&mut self, id: u32) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::uid(self.native_mut(), id);
		self
	}

	/// Set the child process's group ID and make the command native-only.
	pub fn gid(&mut self, id: u32) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::gid(self.native_mut(), id);
		self
	}

	/// Set the child process's `argv[0]` and make the command native-only.
	pub fn arg0(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::arg0(self.native_mut(), arg);
		self
	}

	/// Set the child process's process group and make the command native-only.
	pub fn process_group(&mut self, pgroup: i32) -> &mut Self {
		use ::std::os::unix::process::CommandExt;
		CommandExt::process_group(self.native_mut(), pgroup);
		self
	}

	/// Register a callback to run in the child after `fork` and make the command native-only.
	///
	/// # Safety
	///
	/// The callback runs in the child process after `fork` and before `exec`. It may only perform
	/// operations which are valid in that constrained environment. In particular, allocating or
	/// acquiring locks can be unsound when another thread held the corresponding state across `fork`.
	pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Self
	where
		F: FnMut() -> ::std::io::Result<()> + Send + Sync + 'static,
	{
		use ::std::os::unix::process::CommandExt;
		// SAFETY: the caller accepts the native `pre_exec` contract documented above.
		unsafe { CommandExt::pre_exec(self.native_mut(), f) };
		self
	}
}

#[cfg(all(feature = "std", windows))]
impl Command<Blocking> {
	/// Set Windows process creation flags and make the command native-only.
	pub fn creation_flags(&mut self, flags: u32) -> &mut Self {
		use ::std::os::windows::process::CommandExt;
		CommandExt::creation_flags(self.native_mut(), flags);
		self
	}
}

#[cfg(feature = "tokio1")]
impl Command<Tokio1> {
	/// Configure whether dropping the Tokio child kills it and make the command native-only.
	pub fn kill_on_drop(&mut self, kill_on_drop: bool) -> &mut Self {
		self.native_mut().kill_on_drop(kill_on_drop);
		self
	}
}

#[cfg(all(feature = "tokio1", unix))]
impl Command<Tokio1> {
	/// Set the child process's user ID and make the command native-only.
	pub fn uid(&mut self, id: u32) -> &mut Self {
		self.native_mut().uid(id);
		self
	}

	/// Set the child process's group ID and make the command native-only.
	pub fn gid(&mut self, id: u32) -> &mut Self {
		self.native_mut().gid(id);
		self
	}

	/// Set the child process's `argv[0]` and make the command native-only.
	pub fn arg0(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
		self.native_mut().arg0(arg);
		self
	}

	/// Register a callback to run in the child after `fork` and make the command native-only.
	///
	/// # Safety
	///
	/// The callback runs in the child process after `fork` and before `exec`. It may only perform
	/// operations which are valid in that constrained environment. In particular, allocating or
	/// acquiring locks can be unsound when another thread held the corresponding state across `fork`.
	pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Self
	where
		F: FnMut() -> ::std::io::Result<()> + Send + Sync + 'static,
	{
		// SAFETY: the caller accepts the native `pre_exec` contract documented above.
		unsafe { self.native_mut().pre_exec(f) };
		self
	}
}

#[cfg(all(feature = "tokio1", windows))]
impl Command<Tokio1> {
	/// Set Windows process creation flags and make the command native-only.
	pub fn creation_flags(&mut self, flags: u32) -> &mut Self {
		self.native_mut().creation_flags(flags);
		self
	}
}

#[cfg(all(test, windows))]
mod windows_tests {
	use std::{
		ffi::{OsStr, OsString},
		os::windows::ffi::OsStringExt,
		path::Path,
		process::Stdio,
	};

	use super::{
		CREATE_SUSPENDED_FLAG, CommandArg, CommandIntent, NativeCommand, WindowsSpawnPolicy,
	};

	#[derive(Debug, Eq, PartialEq)]
	enum RecordedArg {
		Regular(OsString),
		Raw(OsString),
	}

	#[derive(Debug)]
	struct RecordedCommand {
		program: OsString,
		args: Vec<RecordedArg>,
	}

	impl NativeCommand for RecordedCommand {
		fn new(program: &OsStr) -> Self {
			Self {
				program: program.to_owned(),
				args: Vec::new(),
			}
		}

		fn arg(&mut self, arg: &OsStr) {
			self.args.push(RecordedArg::Regular(arg.to_owned()));
		}

		fn raw_arg(&mut self, arg: &OsStr) {
			self.args.push(RecordedArg::Raw(arg.to_owned()));
		}

		fn env(&mut self, _key: &OsStr, _value: &OsStr) {}

		fn env_remove(&mut self, _key: &OsStr) {}

		fn env_clear(&mut self) {}

		fn current_dir(&mut self, _dir: &Path) {}

		fn stdin(&mut self, _stdio: Stdio) {}

		fn stdout(&mut self, _stdio: Stdio) {}

		fn stderr(&mut self, _stdio: Stdio) {}

		fn creation_flags(&mut self, _flags: u32) {}

		fn get_program(&self) -> &OsStr {
			&self.program
		}

		fn get_args(&self) -> Box<dyn Iterator<Item = &OsStr> + '_> {
			Box::new(self.args.iter().map(|arg| match arg {
				RecordedArg::Regular(value) | RecordedArg::Raw(value) => value.as_os_str(),
			}))
		}

		fn get_envs(&self) -> Box<dyn Iterator<Item = (&OsStr, Option<&OsStr>)> + '_> {
			Box::new(std::iter::empty())
		}

		fn get_current_dir(&self) -> Option<&Path> {
			None
		}
	}

	#[test]
	fn materialization_preserves_raw_argument_kinds_and_wtf16() {
		let raw = OsString::from_wide(&[b' ' as u16, 0xd800, b' ' as u16]);
		let regular_surrogate = OsString::from_wide(&[0xdfff]);
		let intent = CommandIntent {
			program: OsString::from("tool"),
			args: vec![
				CommandArg::Regular(OsString::from("regular")),
				CommandArg::Raw(raw.clone()),
				CommandArg::Regular(regular_surrogate.clone()),
			],
			env_clear: false,
			env: Vec::new(),
			current_dir: None,
		};

		let command = intent.materialize::<RecordedCommand>();

		assert_eq!(
			command.args,
			[
				RecordedArg::Regular(OsString::from("regular")),
				RecordedArg::Raw(raw),
				RecordedArg::Regular(regular_surrogate),
			]
		);
	}

	#[test]
	fn windows_policy_preserves_flags_without_a_job() {
		let flags = 0x0000_0200 | 0x0800_0000;
		let mut policy = WindowsSpawnPolicy::default();
		policy.set_creation_flags(flags);

		assert!(policy.has_creation_flags());
		assert_eq!(policy.user_creation_flags(), flags);
		assert_eq!(policy.spawn_creation_flags(), flags);
		assert!(!policy.has_job_object());
		assert!(!policy.is_explicitly_suspended());
		assert!(!policy.is_temporarily_suspended());
		assert!(!policy.starts_suspended());
	}

	#[test]
	fn windows_policy_adds_only_temporary_job_suspension() {
		let flags = 0x0000_0200 | 0x0800_0000;
		let mut policy = WindowsSpawnPolicy::default();
		policy.set_job_object();
		policy.set_creation_flags(flags);

		assert_eq!(policy.user_creation_flags(), flags);
		assert_eq!(policy.spawn_creation_flags(), flags | CREATE_SUSPENDED_FLAG);
		assert!(policy.has_job_object());
		assert!(!policy.is_explicitly_suspended());
		assert!(policy.is_temporarily_suspended());
		assert!(policy.starts_suspended());
	}

	#[test]
	fn windows_policy_preserves_explicit_suspension_and_kill_on_drop() {
		let flags = 0x0800_0000 | CREATE_SUSPENDED_FLAG;
		let mut policy = WindowsSpawnPolicy::default();
		policy.set_creation_flags(flags);
		policy.set_job_object();
		policy.set_kill_on_drop(true);

		assert_eq!(policy.user_creation_flags(), flags);
		assert_eq!(policy.spawn_creation_flags(), flags);
		assert!(policy.is_explicitly_suspended());
		assert!(!policy.is_temporarily_suspended());
		assert!(policy.starts_suspended());
		assert!(policy.kills_on_drop());
	}
}
