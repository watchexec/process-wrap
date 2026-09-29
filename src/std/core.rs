use std::{
	any::Any,
	io::{Read, Result},
	process::{
		Child, ChildStderr, ChildStdin, ChildStdout, Command as NativeCommand, ExitStatus, Output,
	},
};

#[cfg(windows)]
use std::os::windows::io::{AsHandle, BorrowedHandle};

#[cfg(all(unix, feature = "process-group"))]
use nix::sys::signal::killpg;
#[cfg(unix)]
use nix::{
	sys::signal::{Signal, kill},
	unistd::Pid,
};

#[cfg(windows)]
macro_rules! prepared_owner_child_contract {
	() => {
		fn stdin(&mut self) -> &mut Option<ChildStdin> {
			self.child_mut().stdin()
		}

		fn stdout(&mut self) -> &mut Option<ChildStdout> {
			self.child_mut().stdout()
		}

		fn stderr(&mut self) -> &mut Option<ChildStderr> {
			self.child_mut().stderr()
		}

		fn id(&self) -> u32 {
			self.child_ref().id()
		}

		fn kill(&mut self) -> Result<()> {
			self.child_mut().kill()
		}

		fn start_kill(&mut self) -> Result<()> {
			self.child_mut().start_kill()
		}

		fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
			self.child_mut().try_wait()
		}

		fn wait(&mut self) -> Result<ExitStatus> {
			self.child_mut().wait()
		}

		fn wait_with_output(mut self: Box<Self>) -> Result<Output> {
			let child = self
				.take_child()
				.expect("a prepared-owner sidecar retains its child");
			let output = child.wait_with_output();
			drop(self);
			output
		}

		#[cfg(unix)]
		fn signal(&mut self, sig: i32) -> Result<()> {
			self.child_mut().signal(sig)
		}
	};
}

crate::generic_wrap::Wrap!(
	crate::Blocking,
	NativeCommand,
	Child,
	ChildWrapper,
	|child| child,
	"std",
	prepared_owner_child_contract
);

/// Wrapper for `std::process::Child`.
///
/// This trait exposes most of the functionality of the underlying [`Child`]. It is implemented for
/// [`Child`] and by wrappers.
///
/// The required methods are `inner`, `inner_mut`, and `into_inner`. Together they expose each lower
/// layer, allowing wrappers to be unwrapped and the native [`Child`] to be used directly when
/// necessary.
///
/// Each non-terminal wrapper must use them to expose its direct lower layer. A terminal non-native
/// child returns itself from all three methods. Wrapper chains must otherwise be acyclic and
/// terminate in either a native [`Child`] or a self-returning non-native child.
///
/// The `try_inner_child`, `try_inner_child_mut`, and `try_into_inner_child` convenience methods on
/// the trait object traverse these layers when access to a native [`Child`] is required.
///
/// It also makes it possible for all the other methods to have default implementations. Some are
/// direct passthroughs to the lower layers, while others are more complex.
///
/// Here's a simple example of a wrapper:
///
/// ```rust
/// use process_wrap::std::*;
/// use std::process::Child;
///
/// #[derive(Debug)]
/// pub struct YourChildWrapper(Child);
///
/// impl ChildWrapper for YourChildWrapper {
///     fn inner(&self) -> &dyn ChildWrapper {
///         &self.0
///     }
///
///     fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
///         &mut self.0
///     }
///
///     fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
///         Box::new((*self).0)
///     }
///
///     #[cfg(windows)]
///     fn process_handle(
///         &self,
///     ) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
///         self.0.process_handle()
///     }
/// }
/// ```
pub trait ChildWrapper: Any + std::fmt::Debug + Send + Sync {
	/// Obtain a reference to the wrapped child.
	fn inner(&self) -> &dyn ChildWrapper;

	/// Obtain a mutable reference to the wrapped child.
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper;

	/// Consume the current wrapper and return the wrapped child.
	///
	/// Note that this may disrupt whatever the current wrapper was doing. However, wrappers must
	/// ensure that the wrapped child is in a consistent state when this is called or they are
	/// dropped, so that this is always safe.
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper>;

	/// Borrow the handle for the process represented by this child, if available.
	///
	/// This method is only available on Windows. The returned handle cannot outlive the borrow of
	/// `self`. Transparent child wrappers should override this method and delegate directly to the
	/// child they own. Terminal custom children which do not represent a native process may retain the
	/// default implementation.
	///
	/// Implementations returning `Some` must return a process handle, rather than another kind of
	/// Windows object. A provider child used with `JobObject` must expose this capability; otherwise
	/// process-wrap returns `Unsupported` and makes a best-effort attempt to terminate the child.
	#[cfg(windows)]
	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		None
	}

	/// Resume the exact thread which process-wrap temporarily suspended for job-object assignment.
	///
	/// This method is only available on Windows. A provider which creates the process temporarily
	/// suspended according to `WindowsSpawnPolicy` should retain its primary-thread handle and return
	/// `Some(result)` after attempting one exact resume. `Some(Err(_))` is authoritative and fails the
	/// spawn lifecycle; process-wrap does not then try another resume mechanism. Return `None` only when
	/// no exact capability exists, which lets `JobObject` use its process-wide thread-enumeration
	/// compatibility fallback.
	#[cfg(windows)]
	fn resume_after_job_assignment(&mut self) -> Option<Result<()>> {
		None
	}

	/// Finalize Windows spawn state owned by this child layer.
	///
	/// Process-wrap invokes this internal lifecycle hook after all child wrappers have been installed.
	/// Implementations act only on their own layer; process-wrap traverses the complete chain.
	#[doc(hidden)]
	#[cfg(windows)]
	fn finalize_spawn_layer(&mut self) -> Result<()> {
		Ok(())
	}

	/// Disarm Windows cleanup state owned by this child layer.
	///
	/// Process-wrap invokes this internal hook only after every ordinary spawn finalizer succeeds, so
	/// cleanup remains armed if any earlier finalizer errors or hits an unwinding panic.
	#[doc(hidden)]
	#[cfg(windows)]
	fn disarm_spawn_cleanup_layer(&mut self) -> Result<()> {
		Ok(())
	}

	/// Report whether this layer owns the JobObject rollback guard.
	///
	/// Process-wrap counts owners and disarms every non-owner before a provider transaction commits.
	/// More than one owner fails the lifecycle before commit. The sole owner stays armed until commit
	/// succeeds and is then disarmed as the only post-commit child-layer operation.
	#[doc(hidden)]
	#[cfg(windows)]
	fn owns_job_object_cleanup_layer(&self) -> bool {
		false
	}

	/// Disarm JobObject-phase cleanup state owned by this child layer.
	///
	/// Non-owning hooks run before provider commit. The sole layer identified by
	/// [`ChildWrapper::owns_job_object_cleanup_layer`] runs after commit and must be failure-atomic:
	/// returning an error or unwinding must leave kill-on-close protection armed. If disarming uses a
	/// native state transition, no caller-controlled callback may run after that transition succeeds.
	#[doc(hidden)]
	#[cfg(windows)]
	fn disarm_job_object_layer(&mut self) -> Result<()> {
		Ok(())
	}

	/// Move built-in prepared state into this layer before its private owner sidecar is removed.
	///
	/// This hook is used only during caller-initiated, post-transfer consuming extraction. Custom
	/// layers cannot obtain process-wrap's strong prepared owner through it.
	#[doc(hidden)]
	#[cfg(all(windows, feature = "job-object"))]
	fn retain_prepared_after_sidecar_removal_layer(&mut self) {}

	/// Return the original PID retained by this exact child layer, if it advertises one.
	///
	/// Process-wrap consults this historical identity only while installing `ProcessGroup` or
	/// `ProcessSession` supervision, including when a post-spawn hook has already reaped the child.
	/// It may preserve the PID observed at spawn for that construction step; it is not proof that the
	/// child remains live and must not be used after spawn returns as a signal or kill target or as a
	/// substitute for lower-child synchronized live signalling.
	///
	/// A custom provider child whose live-facing [`ChildWrapper::id`] cannot supply the original PID
	/// when process-wrap receives it must override this method if it advertises composition with
	/// `ProcessGroup` or `ProcessSession`. Other children may retain the default; this contract does not
	/// require every wrapper to preserve historical identity.
	#[doc(hidden)]
	#[cfg(all(unix, feature = "process-group"))]
	fn spawned_id_layer(&self) -> Option<u32> {
		None
	}

	/// Report whether this exact layer can linearize direct-child status refresh with group signalling.
	#[doc(hidden)]
	#[cfg(all(unix, feature = "process-group"))]
	fn has_process_group_signal_layer(&self) -> bool {
		false
	}

	/// Refresh direct-child status and signal `process_group` before releasing this layer's wait custody.
	///
	/// `Ok(Some(status))` means the direct child was already reaped and no signal was issued;
	/// `Ok(None)` means the signal was issued while the child remained unreaped.
	#[doc(hidden)]
	#[cfg(all(unix, feature = "process-group"))]
	fn signal_process_group_layer(
		&mut self,
		_process_group: i32,
		_signal: i32,
	) -> Option<Result<Option<ExitStatus>>> {
		None
	}

	/// Obtain a clone if possible.
	///
	/// Some implementations may make it possible to clone the implementing structure, even though
	/// std's `Child` isn't `Clone`. In those cases, this method should be overridden.
	fn try_clone(&self) -> Option<Box<dyn ChildWrapper>> {
		None
	}

	/// Obtain the `Child`'s stdin.
	///
	/// By default this is a passthrough to the wrapped child.
	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		self.inner_mut().stdin()
	}

	/// Obtain the `Child`'s stdout.
	///
	/// By default this is a passthrough to the wrapped child.
	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		self.inner_mut().stdout()
	}

	/// Obtain the `Child`'s stderr.
	///
	/// By default this is a passthrough to the wrapped child.
	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		self.inner_mut().stderr()
	}

	/// Obtain the `Child`'s process ID.
	///
	/// In general this should be the PID of the top-level spawned process that was spawned
	/// However, that may vary depending on what a wrapper does.
	fn id(&self) -> u32 {
		self.inner().id()
	}

	/// Kill the `Child` and wait for it to exit.
	///
	/// By default this calls `start_kill()` and then `wait()`, which is the same way it is done on
	/// the underlying `Child`, but that way implementing either or both of those methods will use
	/// them when calling `kill()`, instead of requiring a stub implementation.
	fn kill(&mut self) -> Result<()> {
		self.start_kill()?;
		self.wait()?;
		Ok(())
	}

	/// Kill the `Child` without waiting for it to exit.
	///
	/// By default this is:
	/// - on Unix, sending a `SIGKILL` signal to the process;
	/// - otherwise, a passthrough to the underlying `kill()` method.
	///
	/// The `start_kill()` method doesn't exist on std's `Child`, and was introduced by Tokio. This
	/// library uses it to provide a consistent API across both std and Tokio (and because it's a
	/// generally useful API).
	fn start_kill(&mut self) -> Result<()> {
		self.inner_mut().start_kill()
	}

	/// Check if the `Child` has exited without blocking, and if so, return its exit status.
	///
	/// Wrappers must ensure that repeatedly calling this (or other wait methods) after the child
	/// has exited will always return the same result.
	///
	/// By default this is a passthrough to the underlying `Child`.
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.inner_mut().try_wait()
	}

	/// Wait for the `Child` to exit and return its exit status.
	///
	/// Wrappers must ensure that repeatedly calling this (or other wait methods) after the child
	/// has exited will always return the same result.
	///
	/// By default this is a passthrough to the underlying `Child`.
	fn wait(&mut self) -> Result<ExitStatus> {
		self.inner_mut().wait()
	}

	/// Wait for the `Child` to exit and return its exit status and outputs.
	///
	/// Note that this method reads the child's stdout and stderr to completion into memory.
	///
	/// On Unix, this reads from stdout and stderr simultaneously. On other platforms, it reads from
	/// stdout first, then stderr (pull requests welcome to improve this).
	///
	/// By default this is a reimplementation of the std method, so that it can use the wrapper's
	/// `wait()` method instead of the underlying `Child`'s `wait()`.
	fn wait_with_output(mut self: Box<Self>) -> Result<Output>
	where
		Self: 'static,
	{
		drop(self.stdin().take());

		let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
		match (self.stdout().take(), self.stderr().take()) {
			(None, None) => {}
			(Some(mut out), None) => {
				let res = out.read_to_end(&mut stdout);
				res.unwrap();
			}
			(None, Some(mut err)) => {
				let res = err.read_to_end(&mut stderr);
				res.unwrap();
			}
			(Some(out), Some(err)) => {
				let res = read2(out, &mut stdout, err, &mut stderr);
				res.unwrap();
			}
		}

		let status = self.wait()?;
		Ok(Output {
			status,
			stdout,
			stderr,
		})
	}

	/// Send a signal to the `Child`.
	///
	/// This method is only available on Unix. It doesn't exist on std's `Child`, nor on Tokio's. It
	/// was introduced by command-group to abstract over the signal behaviour between process groups
	/// and unwrapped processes.
	#[cfg(unix)]
	fn signal(&mut self, sig: i32) -> Result<()> {
		self.inner_mut().signal(sig)
	}
}

#[cfg(unix)]
fn signal_child_if_running_with(
	child: &mut Child,
	signal: Signal,
	send: impl FnOnce(Pid, Signal) -> std::result::Result<(), nix::errno::Errno>,
) -> Result<Option<ExitStatus>> {
	if let Some(status) = Child::try_wait(child)? {
		return Ok(Some(status));
	}
	let pid = Pid::from_raw(i32::try_from(Child::id(child)).map_err(std::io::Error::other)?);
	send(pid, signal).map_err(std::io::Error::from)?;
	Ok(None)
}

impl ChildWrapper for Child {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}
	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}
	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}
	#[cfg(windows)]
	fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
		Some(self.as_handle())
	}
	fn stdin(&mut self) -> &mut Option<ChildStdin> {
		&mut self.stdin
	}
	fn stdout(&mut self) -> &mut Option<ChildStdout> {
		&mut self.stdout
	}
	fn stderr(&mut self) -> &mut Option<ChildStderr> {
		&mut self.stderr
	}
	fn id(&self) -> u32 {
		Child::id(self)
	}
	#[cfg(all(unix, feature = "process-group"))]
	fn spawned_id_layer(&self) -> Option<u32> {
		Some(Child::id(self))
	}
	#[cfg(all(unix, feature = "process-group"))]
	fn has_process_group_signal_layer(&self) -> bool {
		true
	}
	#[cfg(all(unix, feature = "process-group"))]
	fn signal_process_group_layer(
		&mut self,
		process_group: i32,
		signal: i32,
	) -> Option<Result<Option<ExitStatus>>> {
		let signal = match Signal::try_from(signal) {
			Ok(signal) => signal,
			Err(error) => return Some(Err(error.into())),
		};
		Some(signal_child_if_running_with(self, signal, |_, signal| {
			killpg(Pid::from_raw(process_group), signal)
		}))
	}
	fn start_kill(&mut self) -> Result<()> {
		#[cfg(unix)]
		{
			signal_child_if_running_with(self, Signal::SIGKILL, kill).map(drop)
		}

		#[cfg(not(unix))]
		{
			Child::kill(self)
		}
	}
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		Child::try_wait(self)
	}
	fn wait(&mut self) -> Result<ExitStatus> {
		Child::wait(self)
	}
	#[cfg(unix)]
	fn signal(&mut self, sig: i32) -> Result<()> {
		signal_child_if_running_with(self, Signal::try_from(sig)?, kill).map(drop)
	}
}

fn same_child(left: &dyn ChildWrapper, right: &dyn ChildWrapper) -> bool {
	std::ptr::addr_eq(left, right) && left.type_id() == right.type_id()
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct FinalJobOwner {
	identity: Option<(std::any::TypeId, *mut ())>,
}

#[cfg(windows)]
fn spawn_layer_identity(layer: &mut dyn ChildWrapper) -> (std::any::TypeId, *mut ()) {
	(
		(&*layer as &dyn Any).type_id(),
		std::ptr::from_mut(layer).cast::<()>(),
	)
}

impl dyn ChildWrapper + '_ {
	fn downcast_ref<T: 'static>(&self) -> Option<&T> {
		(self as &dyn Any).downcast_ref()
	}

	fn is_raw_child(&self) -> bool {
		self.downcast_ref::<Child>().is_some()
	}

	#[cfg(all(unix, feature = "process-group"))]
	pub(crate) fn try_spawned_id(&self) -> Option<u32> {
		let mut inner = self;
		loop {
			if let Some(pid) = inner.spawned_id_layer() {
				return Some(pid);
			}
			let next = inner.inner();
			if same_child(inner, next) {
				return None;
			}
			inner = next;
		}
	}

	#[cfg(all(unix, feature = "process-group"))]
	pub(crate) fn has_process_group_signal_capability(&self) -> bool {
		let mut inner = self;
		loop {
			if inner.has_process_group_signal_layer() {
				return true;
			}
			let next = inner.inner();
			if same_child(inner, next) {
				return false;
			}
			inner = next;
		}
	}

	#[cfg(all(unix, feature = "process-group"))]
	pub(crate) fn try_signal_process_group(
		&mut self,
		process_group: i32,
		signal: i32,
	) -> Option<Result<Option<ExitStatus>>> {
		let mut inner = self;
		loop {
			if inner.has_process_group_signal_layer() {
				return inner.signal_process_group_layer(process_group, signal);
			}
			let inner_type = (&*inner as &dyn Any).type_id();
			let inner_ptr = std::ptr::from_mut(inner);
			let next = inner.inner_mut();
			if std::ptr::addr_eq(inner_ptr, std::ptr::from_mut(next))
				&& inner_type == (&*next as &dyn Any).type_id()
			{
				return None;
			}
			inner = next;
		}
	}

	/// Find the first Windows process-handle capability in this wrapper chain.
	///
	/// Unlike [`ChildWrapper::process_handle`], this traverses legacy transparent layers which do not
	/// explicitly delegate the capability. It returns `None` at a self-terminal custom child.
	#[cfg(windows)]
	pub fn try_process_handle(&self) -> Option<BorrowedHandle<'_>> {
		let mut inner = self;
		loop {
			if let Some(handle) = inner.process_handle() {
				return Some(handle);
			}

			let next = inner.inner();
			if same_child(inner, next) {
				return None;
			}
			inner = next;
		}
	}

	/// Try the first exact post-assignment resume capability in this wrapper chain.
	///
	/// Returns `None` only when no layer owns an exact primary-thread resume operation, allowing callers
	/// to use a thread-enumeration compatibility fallback. A returned `Some(Err(_))` is authoritative
	/// and must fail the lifecycle rather than fall back.
	#[cfg(windows)]
	pub fn try_resume_after_job_assignment(&mut self) -> Option<Result<()>> {
		let mut inner = self;
		loop {
			if let Some(result) = inner.resume_after_job_assignment() {
				return Some(result);
			}

			let inner_type = (&*inner as &dyn Any).type_id();
			let inner_ptr = std::ptr::from_mut(inner);
			let next = inner.inner_mut();
			if std::ptr::addr_eq(inner_ptr, std::ptr::from_mut(next))
				&& inner_type == (&*next as &dyn Any).type_id()
			{
				return None;
			}
			inner = next;
		}
	}

	#[cfg(windows)]
	fn visit_spawn_layers(
		&mut self,
		mut visit: impl FnMut(&mut dyn ChildWrapper) -> Result<()>,
	) -> Result<()> {
		let mut inner = self;
		loop {
			visit(inner)?;

			let inner_type = (&*inner as &dyn Any).type_id();
			let inner_ptr = std::ptr::from_mut(inner);
			let next = inner.inner_mut();
			if std::ptr::addr_eq(inner_ptr, std::ptr::from_mut(next))
				&& inner_type == (&*next as &dyn Any).type_id()
			{
				return Ok(());
			}
			inner = next;
		}
	}

	#[cfg(windows)]
	pub(crate) fn finalize_spawn_before_commit(&mut self) -> Result<FinalJobOwner> {
		self.visit_spawn_layers(|inner| inner.finalize_spawn_layer())?;
		self.visit_spawn_layers(|inner| inner.disarm_spawn_cleanup_layer())?;

		let mut owners = 0;
		let mut identity = None;
		self.visit_spawn_layers(|inner| {
			if inner.owns_job_object_cleanup_layer() {
				owners += 1;
				identity.get_or_insert_with(|| spawn_layer_identity(inner));
				Ok(())
			} else {
				inner.disarm_job_object_layer()
			}
		})?;
		if owners > 1 {
			return Err(std::io::Error::other(
				"multiple child layers own JobObject cleanup",
			));
		}
		Ok(FinalJobOwner { identity })
	}

	#[cfg(windows)]
	pub(crate) fn finalize_spawn_final_owner(&mut self, owner: FinalJobOwner) -> Result<()> {
		let Some(identity) = owner.identity else {
			return Ok(());
		};
		let mut inner = self;
		loop {
			if spawn_layer_identity(inner) == identity {
				return inner.disarm_job_object_layer();
			}

			let inner_type = (&*inner as &dyn Any).type_id();
			let inner_ptr = std::ptr::from_mut(inner);
			let next = inner.inner_mut();
			if std::ptr::addr_eq(inner_ptr, std::ptr::from_mut(next))
				&& inner_type == (&*next as &dyn Any).type_id()
			{
				return Err(std::io::Error::other(
					"the captured JobObject cleanup owner left the child chain",
				));
			}
			inner = next;
		}
	}

	#[cfg(all(windows, feature = "job-object"))]
	pub(crate) fn retain_prepared_after_sidecar_removal(&mut self) {
		let mut inner = self;
		loop {
			inner.retain_prepared_after_sidecar_removal_layer();
			let inner_type = (&*inner as &dyn Any).type_id();
			let inner_ptr = std::ptr::from_mut(inner);
			let next = inner.inner_mut();
			if std::ptr::addr_eq(inner_ptr, std::ptr::from_mut(next))
				&& inner_type == (&*next as &dyn Any).type_id()
			{
				return;
			}
			inner = next;
		}
	}

	/// Try to obtain a reference to the underlying native [`Child`].
	///
	/// Returns `None` if the wrapper chain terminates in a non-native child.
	pub fn try_inner_child(&self) -> Option<&Child> {
		let mut inner = self;
		loop {
			if let Some(child) = inner.downcast_ref::<Child>() {
				return Some(child);
			}

			let next = inner.inner();
			if same_child(inner, next) {
				return None;
			}
			inner = next;
		}
	}

	/// Try to obtain a mutable reference to the underlying native [`Child`].
	///
	/// Returns `None` if the wrapper chain terminates in a non-native child.
	///
	/// # Safety
	///
	/// The caller must ensure that using the returned mutable child does not violate invariants
	/// maintained by any wrapper in the chain.
	pub unsafe fn try_inner_child_mut(&mut self) -> Option<&mut Child> {
		let mut inner = self;
		loop {
			if inner.is_raw_child() {
				return (inner as &mut dyn Any).downcast_mut();
			}

			let inner_type = (&*inner as &dyn Any).type_id();
			let inner_ptr = std::ptr::from_mut(inner);
			let next = inner.inner_mut();
			if std::ptr::addr_eq(inner_ptr, std::ptr::from_mut(next))
				&& inner_type == (&*next as &dyn Any).type_id()
			{
				return None;
			}
			inner = next;
		}
	}

	/// Try to consume the wrapper chain and obtain the underlying native [`Child`].
	///
	/// If the chain terminates in a non-native child, returns that terminal child without calling its
	/// `into_inner` method. Wrappers already traversed before reaching it have been consumed.
	///
	/// # Safety
	///
	/// The caller must ensure that removing every traversed wrapper does not violate wrapper
	/// invariants or bypass required cleanup. This also applies when the method returns `Err`, because
	/// wrappers above the returned terminal child have already been consumed.
	pub unsafe fn try_into_inner_child(self: Box<Self>) -> std::result::Result<Child, Box<Self>> {
		let mut inner = self;
		loop {
			if inner.is_raw_child() {
				return match (inner as Box<dyn Any>).downcast::<Child>() {
					Ok(child) => Ok(*child),
					Err(_) => unreachable!("native child type was checked before downcasting"),
				};
			}

			let terminal = {
				let next = inner.inner();
				same_child(inner.as_ref(), next)
			};
			if terminal {
				return Err(inner);
			}
			inner = inner.into_inner();
		}
	}
}

#[cfg(unix)]
fn read2(
	mut out_r: ChildStdout,
	out_v: &mut Vec<u8>,
	mut err_r: ChildStderr,
	err_v: &mut Vec<u8>,
) -> Result<()> {
	use nix::{
		libc,
		poll::{PollFd, PollFlags, PollTimeout, poll},
	};
	use std::{
		io::Error,
		os::fd::{AsRawFd, BorrowedFd},
	};

	let out_fd = out_r.as_raw_fd();
	let err_fd = err_r.as_raw_fd();
	// SAFETY: `out_r` owns this descriptor through every use of the borrow below.
	let out_bfd = unsafe { BorrowedFd::borrow_raw(out_fd) };
	// SAFETY: `err_r` owns this descriptor through every use of the borrow below.
	let err_bfd = unsafe { BorrowedFd::borrow_raw(err_fd) };

	set_nonblocking(out_bfd, true)?;
	set_nonblocking(err_bfd, true)?;

	let mut fds = [
		PollFd::new(out_bfd, PollFlags::POLLIN),
		PollFd::new(err_bfd, PollFlags::POLLIN),
	];

	loop {
		poll(&mut fds, PollTimeout::NONE)?;

		if fds[0].revents().is_some() && read(&mut out_r, out_v)? {
			set_nonblocking(err_bfd, false)?;
			return err_r.read_to_end(err_v).map(drop);
		}
		if fds[1].revents().is_some() && read(&mut err_r, err_v)? {
			set_nonblocking(out_bfd, false)?;
			return out_r.read_to_end(out_v).map(drop);
		}
	}

	fn read(r: &mut impl Read, dst: &mut Vec<u8>) -> Result<bool> {
		match r.read_to_end(dst) {
			Ok(_) => Ok(true),
			Err(e) => {
				if e.raw_os_error() == Some(libc::EWOULDBLOCK)
					|| e.raw_os_error() == Some(libc::EAGAIN)
				{
					Ok(false)
				} else {
					Err(e)
				}
			}
		}
	}

	#[cfg(target_os = "linux")]
	fn set_nonblocking(fd: BorrowedFd, nonblocking: bool) -> Result<()> {
		use nix::errno::Errno;

		let v = nonblocking as libc::c_int;
		// SAFETY: `fd` is live and `v` is a valid `c_int` argument for `FIONBIO`.
		let res = unsafe { libc::ioctl(fd.as_raw_fd(), libc::FIONBIO, &v) };

		Errno::result(res).map_err(Error::from).map(drop)
	}

	#[cfg(not(target_os = "linux"))]
	fn set_nonblocking(fd: BorrowedFd, nonblocking: bool) -> Result<()> {
		use nix::fcntl::{FcntlArg, OFlag, fcntl};

		let mut flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
		flags.set(OFlag::O_NONBLOCK, nonblocking);

		fcntl(fd, FcntlArg::F_SETFL(flags))
			.map_err(Error::from)
			.map(drop)
	}
}

// if you're reading this code and despairing, we'd love
// your contribution of a proper read2 for your platform!
#[cfg(not(unix))]
fn read2(
	mut out_r: ChildStdout,
	out_v: &mut Vec<u8>,
	mut err_r: ChildStderr,
	err_v: &mut Vec<u8>,
) -> Result<()> {
	out_r.read_to_end(out_v)?;
	err_r.read_to_end(err_v)?;
	Ok(())
}

#[cfg(all(test, unix))]
mod signal_tests {
	use std::{
		process::Command,
		sync::atomic::{AtomicUsize, Ordering},
	};

	use super::*;

	struct ChildGuard(Option<Child>);

	impl ChildGuard {
		fn new(child: Child) -> Self {
			Self(Some(child))
		}

		fn child_mut(&mut self) -> &mut Child {
			self.0.as_mut().expect("an armed test guard owns its child")
		}

		fn finish(mut self) -> Result<()> {
			let child = self.child_mut();
			match child.kill() {
				Ok(()) => {}
				Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
				Err(error) => return Err(error),
			}
			let _ = child.wait()?;
			self.0.take();
			Ok(())
		}
	}

	impl Drop for ChildGuard {
		fn drop(&mut self) {
			if let Some(child) = self.0.as_mut() {
				let _ = child.kill();
				let _ = child.wait();
			}
		}
	}

	#[test]
	#[cfg_attr(miri, ignore = "requires a native child process")]
	fn terminal_status_prevents_the_production_signal_seam() -> Result<()> {
		let child = Command::new("true").spawn()?;
		let mut child = ChildGuard::new(child);
		let status = child.child_mut().wait()?;
		let calls = AtomicUsize::new(0);

		let observed = signal_child_if_running_with(child.child_mut(), Signal::SIGCONT, |_, _| {
			calls.fetch_add(1, Ordering::SeqCst);
			Ok(())
		})?;

		assert_eq!(observed, Some(status));
		assert_eq!(calls.load(Ordering::SeqCst), 0);
		assert_eq!(child.child_mut().wait()?, status);
		child.finish()?;
		Ok(())
	}

	#[test]
	#[cfg_attr(miri, ignore = "requires a native child process")]
	fn live_status_reaches_the_production_signal_seam_once() -> Result<()> {
		let child = Command::new("sh").args(["-c", "sleep 30"]).spawn()?;
		let mut child = ChildGuard::new(child);
		let calls = AtomicUsize::new(0);

		let observed = signal_child_if_running_with(child.child_mut(), Signal::SIGCONT, |_, _| {
			calls.fetch_add(1, Ordering::SeqCst);
			Ok(())
		})?;

		assert_eq!(observed, None);
		assert_eq!(calls.load(Ordering::SeqCst), 1);
		child.finish()?;
		Ok(())
	}
}

const _: () = {
	const fn assert_sync<T: ?Sized + Sync>() {}
	assert_sync::<dyn ChildWrapper>();
};
