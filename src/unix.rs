use std::{
	io, ptr,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicI32, Ordering},
	},
};

use nix::libc;

use crate::command::NativeCommand;

const NO_PROCESS_GROUP: i32 = -1;
const LEADER_PROCESS_GROUP: i32 = 0;

pub(crate) fn reset_sigmask() -> io::Result<()> {
	let mut empty = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
	// SAFETY: `empty` points to writable storage for one signal set.
	if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } == -1 {
		return Err(io::Error::last_os_error());
	}
	// SAFETY: `sigemptyset` initialized `empty`; the old mask is not requested.
	let error =
		unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, empty.as_ptr(), ptr::null_mut()) };
	if error != 0 {
		return Err(io::Error::from_raw_os_error(error));
	}
	Ok(())
}

/// Process-group setup requested for one spawn attempt.
#[cfg_attr(not(feature = "process-group"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessGroupTarget {
	/// Make the spawned process the leader of a new process group.
	Leader,
	/// Attach the spawned process to the existing process group with this ID.
	AttachTo(u32),
}

impl ProcessGroupTarget {
	fn as_raw(self) -> io::Result<i32> {
		match self {
			Self::Leader => Ok(LEADER_PROCESS_GROUP),
			Self::AttachTo(0) => Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"an existing process group ID must be positive",
			)),
			Self::AttachTo(pgid) => i32::try_from(pgid).map_err(|_| {
				io::Error::new(
					io::ErrorKind::InvalidInput,
					"process group ID exceeds the platform range",
				)
			}),
		}
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SpawnPolicy {
	pub(crate) process_group: Option<ProcessGroupTarget>,
	pub(crate) process_session: bool,
	pub(crate) reset_sigmask: bool,
}

impl SpawnPolicy {
	#[cfg(feature = "process-group")]
	pub(crate) fn set_process_group(&mut self, target: ProcessGroupTarget) -> io::Result<()> {
		if self.process_session && matches!(target, ProcessGroupTarget::AttachTo(_)) {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"a process cannot join an existing process group and create a new session",
			));
		}
		target.as_raw()?;
		self.process_group = Some(target);
		Ok(())
	}

	#[cfg(feature = "process-session")]
	pub(crate) fn set_process_session(&mut self) -> io::Result<()> {
		if matches!(self.process_group, Some(ProcessGroupTarget::AttachTo(_))) {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"a process cannot join an existing process group and create a new session",
			));
		}
		self.process_session = true;
		Ok(())
	}

	fn is_empty(self) -> bool {
		self.process_group.is_none() && !self.process_session && !self.reset_sigmask
	}
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CommandState(Arc<Dispatcher>);

impl CommandState {
	pub(crate) fn prepare<N: NativeCommand>(
		&self,
		command: &mut N,
		native_only_base: bool,
		policy: SpawnPolicy,
	) {
		if policy.is_empty() {
			self.0.disarm();
			return;
		}

		if !native_only_base || !self.0.has_native_only_callback() {
			self.install(command);
			if native_only_base {
				self.0
					.installed_on_native_only
					.store(true, Ordering::Release);
			}
		}

		self.0.arm(policy);
	}

	fn install<N: NativeCommand>(&self, command: &mut N) {
		let active = Arc::new(AtomicBool::new(true));
		let mut installed = self
			.0
			.active_callback
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if let Some(previous) = installed.replace(Arc::clone(&active)) {
			previous.store(false, Ordering::Release);
		}
		drop(installed);

		let dispatcher = Arc::clone(&self.0);
		// SAFETY: the callback only invokes async-signal-safe Unix process setup functions and
		// reads atomics populated before spawning. It never accesses `active_callback`'s mutex.
		unsafe {
			command.pre_exec(move || {
				if active.load(Ordering::Acquire) {
					dispatcher.run()
				} else {
					Ok(())
				}
			})
		};
	}

	pub(crate) fn invalidate(&self) {
		self.0.invalidate();
	}

	pub(crate) fn disarm(&self) {
		self.0.disarm();
	}
}

#[derive(Debug)]
struct Dispatcher {
	installed_on_native_only: AtomicBool,
	active_callback: Mutex<Option<Arc<AtomicBool>>>,
	process_group: AtomicI32,
	process_session: AtomicBool,
	reset_sigmask: AtomicBool,
}

impl Default for Dispatcher {
	fn default() -> Self {
		Self {
			installed_on_native_only: AtomicBool::new(false),
			active_callback: Mutex::new(None),
			process_group: AtomicI32::new(NO_PROCESS_GROUP),
			process_session: AtomicBool::new(false),
			reset_sigmask: AtomicBool::new(false),
		}
	}
}

impl Dispatcher {
	fn has_native_only_callback(&self) -> bool {
		if !self.installed_on_native_only.load(Ordering::Acquire) {
			return false;
		}

		let installed = self
			.active_callback
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		// `active_callback` owns one strong reference and the native command's callback owns
		// another. The second reference disappears if an explicit spawner replaces the command.
		installed
			.as_ref()
			.is_some_and(|active| Arc::strong_count(active) > 1)
	}

	fn arm(&self, policy: SpawnPolicy) {
		let process_group = policy
			.process_group
			.map(ProcessGroupTarget::as_raw)
			.transpose()
			.expect("process group targets are validated when configured")
			.unwrap_or(NO_PROCESS_GROUP);
		self.process_group.store(process_group, Ordering::SeqCst);
		self.process_session
			.store(policy.process_session, Ordering::SeqCst);
		self.reset_sigmask
			.store(policy.reset_sigmask, Ordering::SeqCst);
	}

	fn invalidate(&self) {
		self.installed_on_native_only
			.store(false, Ordering::Release);
		if let Some(active) = self
			.active_callback
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
		{
			active.store(false, Ordering::Release);
		}
		self.disarm();
	}

	fn disarm(&self) {
		self.process_group.store(NO_PROCESS_GROUP, Ordering::SeqCst);
		self.process_session.store(false, Ordering::SeqCst);
		self.reset_sigmask.store(false, Ordering::SeqCst);
	}

	fn run(&self) -> io::Result<()> {
		let reset_sigmask = self.reset_sigmask.load(Ordering::SeqCst);
		let process_session = self.process_session.load(Ordering::SeqCst);
		let process_group = self.process_group.load(Ordering::SeqCst);

		if reset_sigmask {
			crate::unix::reset_sigmask()?;
		}

		if process_session {
			// SAFETY: `setsid` takes no pointers and runs in the child before exec.
			if unsafe { libc::setsid() } == -1 {
				return Err(io::Error::last_os_error());
			}
		}

		if process_group != NO_PROCESS_GROUP
			&& !(process_session && process_group == LEADER_PROCESS_GROUP)
		{
			// SAFETY: `setpgid` is applied to the current child and retains no pointers.
			if unsafe { libc::setpgid(0, process_group) } == -1 {
				return Err(io::Error::last_os_error());
			}
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use std::{
		ffi::{OsStr, OsString},
		fmt,
		path::{Path, PathBuf},
		process::Stdio,
	};

	use super::*;

	#[derive(Default)]
	struct FakeCommand {
		program: OsString,
		args: Vec<OsString>,
		env: Vec<(OsString, Option<OsString>)>,
		current_dir: Option<PathBuf>,
		callbacks: Vec<Box<dyn FnMut() -> io::Result<()> + Send + Sync>>,
	}

	impl fmt::Debug for FakeCommand {
		fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
			formatter
				.debug_struct("FakeCommand")
				.field("program", &self.program)
				.field("callbacks", &self.callbacks.len())
				.finish_non_exhaustive()
		}
	}

	impl NativeCommand for FakeCommand {
		fn new(program: &OsStr) -> Self {
			Self {
				program: program.to_owned(),
				..Self::default()
			}
		}

		fn arg(&mut self, arg: &OsStr) {
			self.args.push(arg.to_owned());
		}

		fn env(&mut self, key: &OsStr, value: &OsStr) {
			self.env.push((key.to_owned(), Some(value.to_owned())));
		}

		fn env_remove(&mut self, key: &OsStr) {
			self.env.push((key.to_owned(), None));
		}

		fn env_clear(&mut self) {
			self.env.clear();
		}

		fn current_dir(&mut self, dir: &Path) {
			self.current_dir = Some(dir.to_owned());
		}

		fn stdin(&mut self, _stdio: Stdio) {}

		fn stdout(&mut self, _stdio: Stdio) {}

		fn stderr(&mut self, _stdio: Stdio) {}

		unsafe fn pre_exec<F>(&mut self, callback: F)
		where
			F: FnMut() -> io::Result<()> + Send + Sync + 'static,
		{
			self.callbacks.push(Box::new(callback));
		}

		fn get_program(&self) -> &OsStr {
			&self.program
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

		fn get_current_dir(&self) -> Option<&Path> {
			self.current_dir.as_deref()
		}
	}

	#[test]
	fn native_only_dispatcher_is_reused_until_the_command_is_replaced() {
		let state = CommandState::default();
		let policy = SpawnPolicy {
			process_group: Some(ProcessGroupTarget::Leader),
			..SpawnPolicy::default()
		};
		let mut command = FakeCommand::new(OsStr::new("test"));

		for _ in 0..4 {
			state.prepare(&mut command, true, policy);
			assert_eq!(command.callbacks.len(), 1);
		}

		command = FakeCommand::new(OsStr::new("replacement"));
		state.prepare(&mut command, true, policy);
		assert_eq!(command.callbacks.len(), 1);
	}
}
