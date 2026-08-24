#![cfg(all(any(feature = "std", feature = "tokio1"), any(unix, windows)))]

#[cfg(feature = "std")]
mod std_frontend {
	use std::{
		any::TypeId,
		process::{Child, Command},
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
	};

	use process_wrap::std::ChildWrapper;
	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	use process_wrap::std::{CommandWrap, CommandWrapper};

	#[derive(Debug)]
	struct Layer {
		inner: Option<Box<dyn ChildWrapper>>,
		drops: Arc<AtomicUsize>,
	}

	impl Layer {
		fn new(inner: Box<dyn ChildWrapper>) -> Self {
			Self::tracked(inner, Arc::new(AtomicUsize::new(0)))
		}

		fn tracked(inner: Box<dyn ChildWrapper>, drops: Arc<AtomicUsize>) -> Self {
			Self {
				inner: Some(inner),
				drops,
			}
		}

		fn child(&self) -> &dyn ChildWrapper {
			self.inner
				.as_deref()
				.expect("the layer still owns its child")
		}

		fn child_mut(&mut self) -> &mut dyn ChildWrapper {
			self.inner
				.as_deref_mut()
				.expect("the layer still owns its child")
		}
	}

	impl Drop for Layer {
		fn drop(&mut self) {
			self.drops.fetch_add(1, Ordering::SeqCst);
		}
	}

	impl ChildWrapper for Layer {
		fn inner(&self) -> &dyn ChildWrapper {
			self.child()
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.child_mut()
		}

		fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.inner.take().expect("the layer still owns its child")
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.child().process_handle()
		}
	}

	#[derive(Debug, Default)]
	struct LeafCalls {
		inner: AtomicUsize,
		inner_mut: AtomicUsize,
		into_inner: AtomicUsize,
		drops: AtomicUsize,
	}

	#[derive(Debug)]
	struct Leaf(Arc<LeafCalls>);

	impl Drop for Leaf {
		fn drop(&mut self) {
			self.0.drops.fetch_add(1, Ordering::SeqCst);
		}
	}

	impl ChildWrapper for Leaf {
		fn inner(&self) -> &dyn ChildWrapper {
			self.0.inner.fetch_add(1, Ordering::SeqCst);
			self
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.0.inner_mut.fetch_add(1, Ordering::SeqCst);
			self
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.0.into_inner.fetch_add(1, Ordering::SeqCst);
			self
		}
	}

	#[repr(transparent)]
	#[derive(Debug)]
	struct InlineLayer(Child);

	impl ChildWrapper for InlineLayer {
		fn inner(&self) -> &dyn ChildWrapper {
			&self.0
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			&mut self.0
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			Box::new(self.0)
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.0.process_handle()
		}
	}

	#[derive(Debug)]
	enum ReusedChild {
		Layer(Box<ReusingLayer>),
		Native(Box<Child>),
		Taken,
	}

	#[derive(Debug)]
	struct ReusingLayer {
		inner: ReusedChild,
		into_inner_calls: Arc<AtomicUsize>,
	}

	impl ChildWrapper for ReusingLayer {
		fn inner(&self) -> &dyn ChildWrapper {
			match &self.inner {
				ReusedChild::Layer(child) => child.as_ref(),
				ReusedChild::Native(child) => child.as_ref(),
				ReusedChild::Taken => unreachable!("the layer still owns its child"),
			}
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			match &mut self.inner {
				ReusedChild::Layer(child) => child.as_mut(),
				ReusedChild::Native(child) => child.as_mut(),
				ReusedChild::Taken => unreachable!("the layer still owns its child"),
			}
		}

		fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.into_inner_calls.fetch_add(1, Ordering::SeqCst);
			match std::mem::replace(&mut self.inner, ReusedChild::Taken) {
				ReusedChild::Layer(child) => {
					*self = *child;
					self
				}
				ReusedChild::Native(child) => child,
				ReusedChild::Taken => unreachable!("the layer still owns its child"),
			}
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.inner().process_handle()
		}
	}

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	#[derive(Debug)]
	struct MarkerCommand;

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	#[derive(Debug)]
	struct MarkerChild(Box<dyn ChildWrapper>);

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	impl CommandWrapper for MarkerCommand {
		fn wrap_child(
			&mut self,
			child: Box<dyn ChildWrapper>,
			_core: &CommandWrap,
		) -> std::io::Result<Box<dyn ChildWrapper>> {
			Ok(Box::new(MarkerChild(child)))
		}
	}

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	impl ChildWrapper for MarkerChild {
		fn inner(&self) -> &dyn ChildWrapper {
			self.0.as_ref()
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.0.as_mut()
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.0
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.0.process_handle()
		}
	}

	fn native_child() -> Child {
		#[cfg(unix)]
		return Command::new("sh")
			.args(["-c", "exit 0"])
			.spawn()
			.expect("spawn native child");

		#[cfg(windows)]
		return Command::new("cmd.exe")
			.args(["/D", "/S", "/C", "exit /b 0"])
			.spawn()
			.expect("spawn native child");
	}

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	fn wrapped_command() -> CommandWrap {
		#[cfg(unix)]
		return CommandWrap::with_new("sh", |command| {
			command.args(["-c", "exit 0"]);
		});

		#[cfg(windows)]
		return CommandWrap::with_new("cmd.exe", |command| {
			command.args(["/D", "/S", "/C", "exit /b 0"]);
		});
	}

	fn layered_native() -> Box<dyn ChildWrapper> {
		Box::new(Layer::new(Box::new(Layer::new(Box::new(native_child())))))
	}

	fn reusing_native() -> (Box<dyn ChildWrapper>, Arc<AtomicUsize>) {
		let calls = Arc::new(AtomicUsize::new(0));
		let inner = ReusingLayer {
			inner: ReusedChild::Native(Box::new(native_child())),
			into_inner_calls: Arc::clone(&calls),
		};
		let outer = ReusingLayer {
			inner: ReusedChild::Layer(Box::new(inner)),
			into_inner_calls: Arc::clone(&calls),
		};
		(Box::new(outer), calls)
	}

	fn leaf() -> (Box<dyn ChildWrapper>, Arc<LeafCalls>) {
		let calls = Arc::new(LeafCalls::default());
		(Box::new(Leaf(Arc::clone(&calls))), calls)
	}

	fn data_ptr(child: &dyn ChildWrapper) -> *const () {
		child as *const dyn ChildWrapper as *const ()
	}

	fn is_type<T: 'static>(child: &dyn ChildWrapper) -> bool {
		child.type_id() == TypeId::of::<T>()
	}

	#[test]
	fn native_child_try_accessors_traverse_layers() {
		let mut child = layered_native();
		assert!(child.try_inner_child().is_some());
		child.wait().expect("reap immutable-test child");

		let mut child = layered_native();
		assert!(unsafe { child.try_inner_child_mut() }.is_some());
		child.wait().expect("reap mutable-test child");

		let child = layered_native();
		let mut child = unsafe { child.try_into_inner_child() }.expect("native child");
		child.wait().expect("reap consuming-test child");
	}

	#[test]
	fn inline_native_child_is_not_mistaken_for_a_self_leaf() {
		let mut child: Box<dyn ChildWrapper> = Box::new(InlineLayer(native_child()));
		assert!(child.try_inner_child().is_some());
		child.wait().expect("reap immutable-test child");

		let mut child: Box<dyn ChildWrapper> = Box::new(InlineLayer(native_child()));
		assert!(unsafe { child.try_inner_child_mut() }.is_some());
		child.wait().expect("reap mutable-test child");

		let child: Box<dyn ChildWrapper> = Box::new(InlineLayer(native_child()));
		let mut child = unsafe { child.try_into_inner_child() }.expect("native child");
		child.wait().expect("reap consuming-test child");
	}

	#[test]
	fn consuming_traversal_allows_same_type_to_reuse_its_allocation() {
		let (child, into_inner_calls) = reusing_native();
		let mut child = unsafe { child.try_into_inner_child() }.expect("native child");
		child.wait().expect("reap consuming-test child");
		assert_eq!(into_inner_calls.load(Ordering::SeqCst), 2);
	}

	#[test]
	fn self_leaf_try_accessors_preserve_ownership() {
		let (mut child, calls) = leaf();
		assert!(child.try_inner_child().is_none());
		assert!(unsafe { child.try_inner_child_mut() }.is_none());

		let original = data_ptr(child.as_ref());
		let child = unsafe { child.try_into_inner_child() }
			.expect_err("a non-native leaf must be returned");
		assert_eq!(data_ptr(child.as_ref()), original);
		assert!(is_type::<Leaf>(child.as_ref()));
		assert_eq!(calls.inner.load(Ordering::SeqCst), 2);
		assert_eq!(calls.inner_mut.load(Ordering::SeqCst), 1);
		assert_eq!(calls.into_inner.load(Ordering::SeqCst), 0);
		assert_eq!(calls.drops.load(Ordering::SeqCst), 0);
		drop(child);
		assert_eq!(calls.drops.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn nested_non_native_leaf_is_returned_after_layers_are_consumed() {
		let (leaf, calls) = leaf();
		let leaf_ptr = data_ptr(leaf.as_ref());
		let layer_drops = Arc::new(AtomicUsize::new(0));
		let mut child: Box<dyn ChildWrapper> = Box::new(Layer::tracked(
			Box::new(Layer::tracked(leaf, Arc::clone(&layer_drops))),
			Arc::clone(&layer_drops),
		));

		assert!(child.try_inner_child().is_none());
		assert!(unsafe { child.try_inner_child_mut() }.is_none());
		let child = unsafe { child.try_into_inner_child() }
			.expect_err("the terminal leaf must be returned");
		assert_eq!(data_ptr(child.as_ref()), leaf_ptr);
		assert!(is_type::<Leaf>(child.as_ref()));
		assert_eq!(layer_drops.load(Ordering::SeqCst), 2);
		assert_eq!(calls.inner.load(Ordering::SeqCst), 2);
		assert_eq!(calls.inner_mut.load(Ordering::SeqCst), 1);
		assert_eq!(calls.into_inner.load(Ordering::SeqCst), 0);
		drop(child);
		assert_eq!(calls.drops.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn lifecycle_is_repeatable_through_custom_layers() {
		let mut child = layered_native();
		let first = child.wait().expect("first wait");
		let second = child.wait().expect("second wait");
		let third = child
			.try_wait()
			.expect("try_wait")
			.expect("child has exited");
		assert!(first.success());
		assert_eq!(first, second);
		assert_eq!(first, third);
	}

	#[cfg(all(unix, feature = "process-group"))]
	#[test]
	fn process_group_exposes_its_immediate_custom_child() {
		use process_wrap::std::ProcessGroup;

		let mut command = wrapped_command();
		command.wrap(MarkerCommand).wrap(ProcessGroup::leader());
		let mut child = command.spawn().expect("spawn process group");
		assert!(is_type::<MarkerChild>(child.inner()));
		assert!(is_type::<MarkerChild>(child.inner_mut()));
		let mut child = child.into_inner();
		assert!(is_type::<MarkerChild>(child.as_ref()));
		assert!(child.wait().expect("reap process-group child").success());
	}

	#[cfg(all(unix, feature = "process-session"))]
	#[test]
	fn process_session_exposes_its_immediate_custom_child() {
		use process_wrap::std::ProcessSession;

		let mut command = wrapped_command();
		command.wrap(MarkerCommand).wrap(ProcessSession);
		let mut child = command.spawn().expect("spawn process session");
		assert!(is_type::<MarkerChild>(child.inner()));
		assert!(is_type::<MarkerChild>(child.inner_mut()));
		let mut child = child.into_inner();
		assert!(is_type::<MarkerChild>(child.as_ref()));
		assert!(child.wait().expect("reap process-session child").success());
	}
}

#[cfg(feature = "tokio1")]
mod tokio_frontend {
	use std::{
		any::TypeId,
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
	};

	use process_wrap::tokio::ChildWrapper;
	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	use process_wrap::tokio::{CommandWrap, CommandWrapper};
	use tokio::process::{Child, Command};

	#[derive(Debug)]
	struct Layer {
		inner: Option<Box<dyn ChildWrapper>>,
		drops: Arc<AtomicUsize>,
	}

	impl Layer {
		fn new(inner: Box<dyn ChildWrapper>) -> Self {
			Self::tracked(inner, Arc::new(AtomicUsize::new(0)))
		}

		fn tracked(inner: Box<dyn ChildWrapper>, drops: Arc<AtomicUsize>) -> Self {
			Self {
				inner: Some(inner),
				drops,
			}
		}

		fn child(&self) -> &dyn ChildWrapper {
			self.inner
				.as_deref()
				.expect("the layer still owns its child")
		}

		fn child_mut(&mut self) -> &mut dyn ChildWrapper {
			self.inner
				.as_deref_mut()
				.expect("the layer still owns its child")
		}
	}

	impl Drop for Layer {
		fn drop(&mut self) {
			self.drops.fetch_add(1, Ordering::SeqCst);
		}
	}

	impl ChildWrapper for Layer {
		fn inner(&self) -> &dyn ChildWrapper {
			self.child()
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.child_mut()
		}

		fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.inner.take().expect("the layer still owns its child")
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.child().process_handle()
		}
	}

	#[derive(Debug, Default)]
	struct LeafCalls {
		inner: AtomicUsize,
		inner_mut: AtomicUsize,
		into_inner: AtomicUsize,
		drops: AtomicUsize,
	}

	#[derive(Debug)]
	struct Leaf(Arc<LeafCalls>);

	impl Drop for Leaf {
		fn drop(&mut self) {
			self.0.drops.fetch_add(1, Ordering::SeqCst);
		}
	}

	impl ChildWrapper for Leaf {
		fn inner(&self) -> &dyn ChildWrapper {
			self.0.inner.fetch_add(1, Ordering::SeqCst);
			self
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.0.inner_mut.fetch_add(1, Ordering::SeqCst);
			self
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.0.into_inner.fetch_add(1, Ordering::SeqCst);
			self
		}
	}

	#[repr(transparent)]
	#[derive(Debug)]
	struct InlineLayer(Child);

	impl ChildWrapper for InlineLayer {
		fn inner(&self) -> &dyn ChildWrapper {
			&self.0
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			&mut self.0
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			Box::new(self.0)
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.0.process_handle()
		}
	}

	#[derive(Debug)]
	enum ReusedChild {
		Layer(Box<ReusingLayer>),
		Native(Box<Child>),
		Taken,
	}

	#[derive(Debug)]
	struct ReusingLayer {
		inner: ReusedChild,
		into_inner_calls: Arc<AtomicUsize>,
	}

	impl ChildWrapper for ReusingLayer {
		fn inner(&self) -> &dyn ChildWrapper {
			match &self.inner {
				ReusedChild::Layer(child) => child.as_ref(),
				ReusedChild::Native(child) => child.as_ref(),
				ReusedChild::Taken => unreachable!("the layer still owns its child"),
			}
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			match &mut self.inner {
				ReusedChild::Layer(child) => child.as_mut(),
				ReusedChild::Native(child) => child.as_mut(),
				ReusedChild::Taken => unreachable!("the layer still owns its child"),
			}
		}

		fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.into_inner_calls.fetch_add(1, Ordering::SeqCst);
			match std::mem::replace(&mut self.inner, ReusedChild::Taken) {
				ReusedChild::Layer(child) => {
					*self = *child;
					self
				}
				ReusedChild::Native(child) => child,
				ReusedChild::Taken => unreachable!("the layer still owns its child"),
			}
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.inner().process_handle()
		}
	}

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	#[derive(Debug)]
	struct MarkerCommand;

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	#[derive(Debug)]
	struct MarkerChild(Box<dyn ChildWrapper>);

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	impl CommandWrapper for MarkerCommand {
		fn wrap_child(
			&mut self,
			child: Box<dyn ChildWrapper>,
			_core: &CommandWrap,
		) -> std::io::Result<Box<dyn ChildWrapper>> {
			Ok(Box::new(MarkerChild(child)))
		}
	}

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	impl ChildWrapper for MarkerChild {
		fn inner(&self) -> &dyn ChildWrapper {
			self.0.as_ref()
		}

		fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
			self.0.as_mut()
		}

		fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
			self.0
		}

		#[cfg(windows)]
		fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
			self.0.process_handle()
		}
	}

	fn native_child() -> Child {
		#[cfg(unix)]
		return Command::new("sh")
			.args(["-c", "exit 0"])
			.spawn()
			.expect("spawn native child");

		#[cfg(windows)]
		return Command::new("cmd.exe")
			.args(["/D", "/S", "/C", "exit /b 0"])
			.spawn()
			.expect("spawn native child");
	}

	#[cfg(all(unix, any(feature = "process-group", feature = "process-session")))]
	fn wrapped_command() -> CommandWrap {
		#[cfg(unix)]
		return CommandWrap::with_new("sh", |command| {
			command.args(["-c", "exit 0"]);
		});

		#[cfg(windows)]
		return CommandWrap::with_new("cmd.exe", |command| {
			command.args(["/D", "/S", "/C", "exit /b 0"]);
		});
	}

	fn layered_native() -> Box<dyn ChildWrapper> {
		Box::new(Layer::new(Box::new(Layer::new(Box::new(native_child())))))
	}

	fn reusing_native() -> (Box<dyn ChildWrapper>, Arc<AtomicUsize>) {
		let calls = Arc::new(AtomicUsize::new(0));
		let inner = ReusingLayer {
			inner: ReusedChild::Native(Box::new(native_child())),
			into_inner_calls: Arc::clone(&calls),
		};
		let outer = ReusingLayer {
			inner: ReusedChild::Layer(Box::new(inner)),
			into_inner_calls: Arc::clone(&calls),
		};
		(Box::new(outer), calls)
	}

	fn leaf() -> (Box<dyn ChildWrapper>, Arc<LeafCalls>) {
		let calls = Arc::new(LeafCalls::default());
		(Box::new(Leaf(Arc::clone(&calls))), calls)
	}

	fn data_ptr(child: &dyn ChildWrapper) -> *const () {
		child as *const dyn ChildWrapper as *const ()
	}

	fn is_type<T: 'static>(child: &dyn ChildWrapper) -> bool {
		child.type_id() == TypeId::of::<T>()
	}

	#[tokio::test]
	async fn native_child_try_accessors_traverse_layers() {
		let mut child = layered_native();
		assert!(child.try_inner_child().is_some());
		child.wait().await.expect("reap immutable-test child");

		let mut child = layered_native();
		assert!(unsafe { child.try_inner_child_mut() }.is_some());
		child.wait().await.expect("reap mutable-test child");

		let child = layered_native();
		let mut child = unsafe { child.try_into_inner_child() }.expect("native child");
		child.wait().await.expect("reap consuming-test child");
	}

	#[tokio::test]
	async fn inline_native_child_is_not_mistaken_for_a_self_leaf() {
		let mut child: Box<dyn ChildWrapper> = Box::new(InlineLayer(native_child()));
		assert!(child.try_inner_child().is_some());
		child.wait().await.expect("reap immutable-test child");

		let mut child: Box<dyn ChildWrapper> = Box::new(InlineLayer(native_child()));
		assert!(unsafe { child.try_inner_child_mut() }.is_some());
		child.wait().await.expect("reap mutable-test child");

		let child: Box<dyn ChildWrapper> = Box::new(InlineLayer(native_child()));
		let mut child = unsafe { child.try_into_inner_child() }.expect("native child");
		child.wait().await.expect("reap consuming-test child");
	}

	#[tokio::test]
	async fn consuming_traversal_allows_same_type_to_reuse_its_allocation() {
		let (child, into_inner_calls) = reusing_native();
		let mut child = unsafe { child.try_into_inner_child() }.expect("native child");
		child.wait().await.expect("reap consuming-test child");
		assert_eq!(into_inner_calls.load(Ordering::SeqCst), 2);
	}

	#[tokio::test]
	async fn self_leaf_try_accessors_preserve_ownership() {
		let (mut child, calls) = leaf();
		assert!(child.try_inner_child().is_none());
		assert!(unsafe { child.try_inner_child_mut() }.is_none());

		let original = data_ptr(child.as_ref());
		let child = unsafe { child.try_into_inner_child() }
			.expect_err("a non-native leaf must be returned");
		assert_eq!(data_ptr(child.as_ref()), original);
		assert!(is_type::<Leaf>(child.as_ref()));
		assert_eq!(calls.inner.load(Ordering::SeqCst), 2);
		assert_eq!(calls.inner_mut.load(Ordering::SeqCst), 1);
		assert_eq!(calls.into_inner.load(Ordering::SeqCst), 0);
		assert_eq!(calls.drops.load(Ordering::SeqCst), 0);
		drop(child);
		assert_eq!(calls.drops.load(Ordering::SeqCst), 1);
	}

	#[tokio::test]
	async fn nested_non_native_leaf_is_returned_after_layers_are_consumed() {
		let (leaf, calls) = leaf();
		let leaf_ptr = data_ptr(leaf.as_ref());
		let layer_drops = Arc::new(AtomicUsize::new(0));
		let mut child: Box<dyn ChildWrapper> = Box::new(Layer::tracked(
			Box::new(Layer::tracked(leaf, Arc::clone(&layer_drops))),
			Arc::clone(&layer_drops),
		));

		assert!(child.try_inner_child().is_none());
		assert!(unsafe { child.try_inner_child_mut() }.is_none());
		let child = unsafe { child.try_into_inner_child() }
			.expect_err("the terminal leaf must be returned");
		assert_eq!(data_ptr(child.as_ref()), leaf_ptr);
		assert!(is_type::<Leaf>(child.as_ref()));
		assert_eq!(layer_drops.load(Ordering::SeqCst), 2);
		assert_eq!(calls.inner.load(Ordering::SeqCst), 2);
		assert_eq!(calls.inner_mut.load(Ordering::SeqCst), 1);
		assert_eq!(calls.into_inner.load(Ordering::SeqCst), 0);
		drop(child);
		assert_eq!(calls.drops.load(Ordering::SeqCst), 1);
	}

	#[tokio::test]
	async fn lifecycle_is_repeatable_through_custom_layers() {
		let mut child = layered_native();
		let first = child.wait().await.expect("first wait");
		let second = child.wait().await.expect("second wait");
		let third = child
			.try_wait()
			.expect("try_wait")
			.expect("child has exited");
		assert!(first.success());
		assert_eq!(first, second);
		assert_eq!(first, third);
	}

	#[cfg(all(unix, feature = "process-group"))]
	#[tokio::test]
	async fn process_group_exposes_its_immediate_custom_child() {
		use process_wrap::tokio::ProcessGroup;

		let mut command = wrapped_command();
		command.wrap(MarkerCommand).wrap(ProcessGroup::leader());
		let mut child = command.spawn().expect("spawn process group");
		assert!(is_type::<MarkerChild>(child.inner()));
		assert!(is_type::<MarkerChild>(child.inner_mut()));
		let mut child = child.into_inner();
		assert!(is_type::<MarkerChild>(child.as_ref()));
		assert!(
			child
				.wait()
				.await
				.expect("reap process-group child")
				.success()
		);
	}

	#[cfg(all(unix, feature = "process-session"))]
	#[tokio::test]
	async fn process_session_exposes_its_immediate_custom_child() {
		use process_wrap::tokio::ProcessSession;

		let mut command = wrapped_command();
		command.wrap(MarkerCommand).wrap(ProcessSession);
		let mut child = command.spawn().expect("spawn process session");
		assert!(is_type::<MarkerChild>(child.inner()));
		assert!(is_type::<MarkerChild>(child.inner_mut()));
		let mut child = child.into_inner();
		assert!(is_type::<MarkerChild>(child.as_ref()));
		assert!(
			child
				.wait()
				.await
				.expect("reap process-session child")
				.success()
		);
	}
}
