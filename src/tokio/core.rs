use std::{
	any::Any,
	future::Future,
	io::Result,
	pin::Pin,
	process::{ExitStatus, Output},
};

#[cfg(windows)]
use std::os::windows::io::BorrowedHandle;

use futures::future::try_join3;
#[cfg(unix)]
use nix::{
	sys::signal::{Signal, kill},
	unistd::Pid,
};
use tokio::{
	io::{AsyncRead, AsyncReadExt},
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command as NativeCommand},
};

#[cfg(windows)]
macro_rules! prepared_owner_child_contract {
	() => {
		#[cfg(all(
			unix,
			feature = "pty",
			any(feature = "process-group", feature = "process-session")
		))]
		fn spawned_id_layer(&self) -> Option<u32> {
			self.child_ref().spawned_id_layer()
		}

		#[cfg(feature = "pty")]
		fn take_pty_controller_layer(&mut self) -> Option<super::pty::PtyController> {
			self.child_mut().take_pty_controller_layer()
		}

		fn stdin(&mut self) -> &mut Option<ChildStdin> {
			self.child_mut().stdin()
		}

		fn stdout(&mut self) -> &mut Option<ChildStdout> {
			self.child_mut().stdout()
		}

		fn stderr(&mut self) -> &mut Option<ChildStderr> {
			self.child_mut().stderr()
		}

		fn id(&self) -> Option<u32> {
			self.child_ref().id()
		}

		fn kill(&mut self) -> Box<dyn Future<Output = Result<()>> + Send + '_> {
			self.child_mut().kill()
		}

		fn start_kill(&mut self) -> Result<()> {
			self.child_mut().start_kill()
		}

		fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
			self.child_mut().try_wait()
		}

		fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
			self.child_mut().wait()
		}

		fn wait_with_output(
			mut self: Box<Self>,
		) -> Box<dyn Future<Output = Result<Output>> + Send> {
			let child = self
				.take_child()
				.expect("a prepared-owner sidecar retains its child");
			prepared_owner_wait_with_output(child, self)
		}

		#[cfg(unix)]
		fn signal(&self, sig: i32) -> Result<()> {
			self.child_ref().signal(sig)
		}
	};
}

crate::generic_wrap::Wrap!(
	crate::Tokio1,
	NativeCommand,
	Child,
	ChildWrapper,
	|child| { child },
	"tokio",
	prepared_owner_child_contract
);

#[cfg(windows)]
struct PreparedOwnerWaitWithOutput {
	// Field order is intentional: cancellation drops the child future before prepared storage.
	child: Option<Pin<Box<dyn Future<Output = Result<Output>> + Send>>>,
	owner: Option<Box<PreparedOwnerChild>>,
}

#[cfg(windows)]
impl Future for PreparedOwnerWaitWithOutput {
	type Output = Result<Output>;

	fn poll(
		self: Pin<&mut Self>,
		context: &mut std::task::Context<'_>,
	) -> std::task::Poll<Self::Output> {
		let this = self.get_mut();
		let result = match this
			.child
			.as_mut()
			.expect("a prepared wait retains its child future")
			.as_mut()
			.poll(context)
		{
			std::task::Poll::Pending => return std::task::Poll::Pending,
			std::task::Poll::Ready(result) => result,
		};
		drop(this.child.take());
		drop(this.owner.take());
		std::task::Poll::Ready(result)
	}
}

#[cfg(windows)]
fn prepared_owner_wait_with_output(
	child: Box<dyn ChildWrapper>,
	owner: Box<PreparedOwnerChild>,
) -> Box<dyn Future<Output = Result<Output>> + Send> {
	Box::new(PreparedOwnerWaitWithOutput {
		child: Some(Box::into_pin(child.wait_with_output())),
		owner: Some(owner),
	})
}

/// Wrapper for `tokio::process::Child`.
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
/// direct passthroughs to the underlying `Child`, while others are more complex.
///
/// Here's a simple example of a wrapper:
///
/// ```rust
/// use process_wrap::tokio::*;
/// use tokio::process::Child;
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

	/// Return the original PID retained by this exact provider-child layer.
	///
	/// This internal capability lets a wrapper finish installation after an earlier post-spawn hook
	/// observed and reaped a fast provider child. Ordinary child operations must continue to use
	/// [`ChildWrapper::id`] so they never act on a recycled PID.
	#[doc(hidden)]
	#[cfg(all(
		unix,
		feature = "pty",
		any(feature = "process-group", feature = "process-session")
	))]
	fn spawned_id_layer(&self) -> Option<u32> {
		None
	}

	/// Take the PTY controller owned by this exact child layer.
	#[doc(hidden)]
	#[cfg(feature = "pty")]
	fn take_pty_controller_layer(&mut self) -> Option<super::pty::PtyController> {
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

	/// Obtain a clone if possible.
	///
	/// Some implementations may make it possible to clone the implementing structure, even though
	/// Tokio's `Child` isn't `Clone`. In those cases, this method should be overridden.
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
	///
	/// Returns an `Option` to resemble Tokio's API, but isn't expected to be `None` in practice.
	fn id(&self) -> Option<u32> {
		self.inner().id()
	}

	/// Kill the `Child` and wait for it to exit.
	///
	/// By default this calls `start_kill()` and then `wait()`, which is the same way it is done on
	/// the underlying `Child`, but that way implementing either or both of those methods will use
	/// them when calling `kill()`, instead of requiring a stub implementation.
	fn kill(&mut self) -> Box<dyn Future<Output = Result<()>> + Send + '_> {
		Box::new(async {
			self.start_kill()?;
			self.wait().await?;
			Ok(())
		})
	}

	/// Kill the `Child` without waiting for it to exit.
	///
	/// By default this is a passthrough to the underlying `Child`, which:
	/// - on Unix, sends a `SIGKILL` signal to the process;
	/// - otherwise, passes through to the `kill()` method.
	fn start_kill(&mut self) -> Result<()> {
		self.inner_mut().start_kill()
	}

	/// Check if the `Child` has exited without waiting, and if it has, return its exit status.
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
	fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
		Box::pin(self.inner_mut().wait())
	}

	/// Wait for the `Child` to exit and return its exit status and outputs.
	///
	/// Note that this method reads the child's stdout and stderr to completion into memory.
	///
	/// By default this is a reimplementation of the Tokio method, so that it can use the wrapper's
	/// `wait()` method instead of the underlying `Child`'s `wait()`.
	fn wait_with_output(mut self: Box<Self>) -> Box<dyn Future<Output = Result<Output>> + Send>
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

			let (status, stdout, stderr) = try_join3(self.wait(), stdout_fut, stderr_fut).await?;

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

	/// Send a signal to the `Child`.
	///
	/// This method is only available on Unix. It doesn't exist on Tokio's `Child`, nor on std's. It
	/// was introduced by command-group to abstract over the signal behaviour between process groups
	/// and unwrapped processes.
	#[cfg(unix)]
	fn signal(&self, sig: i32) -> Result<()> {
		self.inner().signal(sig)
	}
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
		let handle = self.raw_handle()?;
		// SAFETY: `raw_handle` returns the handle owned by `self`, and the returned borrow cannot
		// outlive `self`.
		Some(unsafe { BorrowedHandle::borrow_raw(handle) })
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
	fn id(&self) -> Option<u32> {
		Child::id(self)
	}
	fn start_kill(&mut self) -> Result<()> {
		Child::start_kill(self)
	}
	fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		Child::try_wait(self)
	}
	fn wait(&mut self) -> Pin<Box<dyn Future<Output = Result<ExitStatus>> + Send + '_>> {
		Box::pin(Child::wait(self))
	}
	#[cfg(unix)]
	fn signal(&self, sig: i32) -> Result<()> {
		if let Some(id) = self.id() {
			kill(
				Pid::from_raw(i32::try_from(id).map_err(std::io::Error::other)?),
				Signal::try_from(sig)?,
			)
			.map_err(std::io::Error::from)
		} else {
			Ok(())
		}
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

	#[cfg(all(
		unix,
		feature = "pty",
		any(feature = "process-group", feature = "process-session")
	))]
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

	/// Take the controller installed by a PTY spawn.
	///
	/// This traverses arbitrary child-wrapper layers without removing them. The controller can be taken
	/// only once; subsequent calls and non-PTY children return `None`.
	#[cfg(feature = "pty")]
	#[cfg_attr(docsrs, doc(cfg(feature = "pty")))]
	pub fn take_pty_controller(&mut self) -> Option<super::pty::PtyController> {
		let mut inner = self;
		loop {
			if let Some(controller) = inner.take_pty_controller_layer() {
				return Some(controller);
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

const _: () = {
	const fn assert_sync<T: ?Sized + Sync>() {}
	assert_sync::<dyn ChildWrapper>();
};

#[cfg(all(test, windows, feature = "job-object"))]
mod prepared_output_wait_tests {
	use std::{
		future::Future,
		io,
		pin::Pin,
		process::{ExitStatus, Output},
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
			mpsc,
		},
		task::{Context, Poll},
		thread,
		time::Duration,
	};

	use futures::task::noop_waker;

	use super::*;

	const TIMEOUT: Duration = Duration::from_secs(5);

	#[derive(Debug)]
	struct PreparedDrop(Arc<AtomicUsize>);

	impl Drop for PreparedDrop {
		fn drop(&mut self) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	#[derive(Debug)]
	struct OutputChild {
		future_drops: Arc<AtomicUsize>,
		ready: bool,
	}

	impl ChildWrapper for OutputChild {
		fn inner(&self) -> &dyn ChildWrapper {
			self
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self
		}

		fn wait_with_output(
			self: Box<Self>,
		) -> Box<dyn Future<Output = io::Result<Output>> + Send> {
			Box::new(OutputFuture {
				drops: Arc::clone(&self.future_drops),
				ready: self.ready,
			})
		}
	}

	struct OutputFuture {
		drops: Arc<AtomicUsize>,
		ready: bool,
	}

	impl Future for OutputFuture {
		type Output = io::Result<Output>;

		fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
			if self.ready {
				use std::os::windows::process::ExitStatusExt;
				Poll::Ready(Ok(Output {
					status: ExitStatus::from_raw(0),
					stdout: Vec::new(),
					stderr: Vec::new(),
				}))
			} else {
				Poll::Pending
			}
		}
	}

	impl Drop for OutputFuture {
		fn drop(&mut self) {
			self.drops.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn assert_output_wait_drops_child_before_blocking_on_owner(complete: bool) {
		let prepared_drops = Arc::new(AtomicUsize::new(0));
		let future_drops = Arc::new(AtomicUsize::new(0));
		let racing_callbacks = Arc::new(AtomicUsize::new(0));
		let owner = PreparedChildOwner::new(Box::new(PreparedDrop(Arc::clone(&prepared_drops))));
		let state = Arc::clone(&owner.state);
		let token = Arc::new(owner.installed_token());
		let child = PreparedOwnerChild::new(
			Box::new(OutputChild {
				future_drops: Arc::clone(&future_drops),
				ready: complete,
			}),
			vec![Some(owner)],
		);
		let output = child.wait_with_output();
		let mut cleanup = PreparedRaceCleanup::new();

		let (entered_tx, entered_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let (inspection_tx, inspection_rx) = mpsc::channel();
		cleanup.releases.push(release_tx.clone());
		let inspection_token = Arc::clone(&token);
		cleanup.workers.push(thread::spawn(move || {
			let completed = inspection_token.with::<PreparedDrop, _>(|_| {
				let _ = entered_tx.send(());
				release_rx.recv().is_ok()
			}) == Some(true);
			let _ = inspection_tx.send(completed);
		}));
		entered_rx
			.recv_timeout(TIMEOUT)
			.expect("prepared inspection entered after upgrading and locking state");

		let (racing_reached_tx, racing_reached_rx) = mpsc::channel();
		let (racing_release_tx, racing_release_rx) = mpsc::channel();
		*state
			.before_value_lock
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PreparedStateTestRendezvous {
			reached: racing_reached_tx,
			release: racing_release_rx,
		});
		cleanup.releases.push(racing_release_tx.clone());
		let (racing_tx, racing_rx) = mpsc::channel();
		let racing_token = Arc::clone(&token);
		let racing_callback_calls = Arc::clone(&racing_callbacks);
		cleanup.workers.push(thread::spawn(move || {
			let accessed = racing_token
				.with::<PreparedDrop, _>(|_| {
					racing_callback_calls.fetch_add(1, Ordering::SeqCst);
				})
				.is_some();
			let _ = racing_tx.send(accessed);
		}));
		racing_reached_rx
			.recv_timeout(TIMEOUT)
			.expect("the racing accessor upgraded before owner closing");

		let (closing_reached_tx, closing_reached_rx) = mpsc::channel();
		let (closing_release_tx, closing_release_rx) = mpsc::channel();
		*state
			.after_closing
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PreparedStateTestRendezvous {
			reached: closing_reached_tx,
			release: closing_release_rx,
		});
		cleanup.releases.push(closing_release_tx.clone());
		racing_release_tx
			.send(())
			.expect("release the racing accessor to contend for the value");

		let (finished_tx, finished_rx) = mpsc::channel();
		cleanup.workers.push(thread::spawn(move || {
			if complete {
				let mut output = Box::into_pin(output);
				let waker = noop_waker();
				let mut context = Context::from_waker(&waker);
				assert!(matches!(
					output.as_mut().poll(&mut context),
					Poll::Ready(Ok(_))
				));
			} else {
				drop(output);
			}
			let _ = finished_tx.send(());
		}));
		closing_reached_rx
			.recv_timeout(TIMEOUT)
			.expect("output-wait disposal published owner closing");
		assert!(
			state
				.admission
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.closing,
			"the output-wait owner reached the authoritative closed state"
		);
		assert_eq!(
			future_drops.load(Ordering::SeqCst),
			1,
			"the child future drops before prepared-owner closing"
		);
		assert_eq!(
			finished_rx.try_recv(),
			Err(mpsc::TryRecvError::Empty),
			"output-wait disposal returned during immutable prepared access"
		);
		assert_eq!(prepared_drops.load(Ordering::SeqCst), 0);

		release_tx
			.send(())
			.expect("release the finite prepared inspection");
		assert!(
			!racing_rx
				.recv_timeout(TIMEOUT)
				.expect("the queued prepared access completed before owner take"),
			"a queued access invoked its callback after owner closing"
		);
		assert_eq!(racing_callbacks.load(Ordering::SeqCst), 0);
		closing_release_tx
			.send(())
			.expect("release output-wait owner disposal");
		finished_rx
			.recv_timeout(TIMEOUT)
			.expect("output-wait disposal completes after prepared inspection release");
		assert_eq!(prepared_drops.load(Ordering::SeqCst), 1);
		assert!(
			token.with::<PreparedDrop, _>(|_| ()).is_none(),
			"a retained token accessed prepared state after owner destruction"
		);
		assert!(
			inspection_rx
				.recv_timeout(TIMEOUT)
				.expect("prepared inspection worker completed")
		);
		cleanup.finish();
	}

	#[test]
	fn cancellation_drops_child_future_before_revoking_prepared_owner() {
		assert_output_wait_drops_child_before_blocking_on_owner(false);
	}

	#[test]
	fn completion_drops_child_future_before_revoking_prepared_owner() {
		assert_output_wait_drops_child_before_blocking_on_owner(true);
	}
}
