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

		if !native_only_base || !self.0.installed_on_native_only.load(Ordering::Acquire) {
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

	/// Require the next native-only attempt to install a fresh dispatcher callback.
	///
	/// The currently active callback remains usable by an explicit spawner which has just received the
	/// native command, but arbitrary mutation may replace and drop it before that spawner returns.
	pub(crate) fn require_reinstall(&self) {
		self.0
			.installed_on_native_only
			.store(false, Ordering::Release);
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
			let mut empty = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
			// SAFETY: `empty` points to writable storage for one signal set.
			if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } == -1 {
				return Err(io::Error::last_os_error());
			}
			// SAFETY: `sigemptyset` initialized `empty`; the old mask is not requested.
			let error = unsafe {
				libc::pthread_sigmask(libc::SIG_SETMASK, empty.as_ptr(), ptr::null_mut())
			};
			if error != 0 {
				return Err(io::Error::from_raw_os_error(error));
			}
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
