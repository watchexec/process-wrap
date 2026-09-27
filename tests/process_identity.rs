#![cfg(all(unix, feature = "tokio1", feature = "process-group"))]

use std::{
	future::Future,
	io,
	os::unix::process::ExitStatusExt,
	pin::Pin,
	process::ExitStatus,
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
		mpsc,
	},
	time::{Duration, Instant},
};

use process_wrap::{
	ProcessGroupTarget, SpawnTransaction,
	tokio::{
		ChildWrapper, Command, CommandWrapper, ProcessGroup, ProviderProduct, SpawnAttempt,
		SpawnProvider,
	},
};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct CompletedTransaction;

impl SpawnTransaction for CompletedTransaction {
	fn commit(&mut self) -> io::Result<()> {
		Ok(())
	}

	fn rollback(&mut self) -> io::Result<()> {
		Ok(())
	}
}

#[derive(Debug)]
struct HistoricalChild {
	child: Arc<Mutex<tokio::process::Child>>,
	spawned_id: u32,
}

impl HistoricalChild {
	fn lock(&self) -> std::sync::MutexGuard<'_, tokio::process::Child> {
		self.child
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
	}
}

impl ChildWrapper for HistoricalChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn id(&self) -> Option<u32> {
		self.lock().id()
	}

	fn spawned_id_layer(&self) -> Option<u32> {
		Some(self.spawned_id)
	}

	fn has_process_group_signal_layer(&self) -> bool {
		true
	}

	fn signal_process_group_layer(
		&mut self,
		process_group: i32,
		signal: i32,
	) -> Option<io::Result<Option<ExitStatus>>> {
		ChildWrapper::signal_process_group_layer(&mut *self.lock(), process_group, signal)
	}

	fn start_kill(&mut self) -> io::Result<()> {
		ChildWrapper::start_kill(&mut *self.lock())
	}

	fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
		self.lock().try_wait()
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
		let child = Arc::clone(&self.child);
		Box::pin(std::future::poll_fn(move |context| {
			let mut child = child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			let mut wait = Box::pin(child.wait());
			wait.as_mut().poll(context)
		}))
	}
}

#[derive(Debug)]
struct HistoricalProvider;

impl SpawnProvider for HistoricalProvider {
	fn spawn(&self, attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<ProviderProduct> {
		assert_eq!(
			attempt.process_group_target(),
			Some(ProcessGroupTarget::Leader)
		);
		let mut command = tokio::process::Command::new("sh");
		command.args(["-c", "exit 29"]).process_group(0);
		let mut child = command.spawn()?;
		let spawned_id = child
			.id()
			.ok_or_else(|| io::Error::other("the provider child has no spawned PID"))?;
		let deadline = Instant::now() + EXIT_TIMEOUT;
		while child.try_wait()?.is_none() {
			if Instant::now() >= deadline {
				return Err(io::Error::new(
					io::ErrorKind::TimedOut,
					"the provider child did not exit",
				));
			}
			std::thread::yield_now();
		}
		assert_eq!(child.id(), None);
		Ok(ProviderProduct::new(
			Box::new(HistoricalChild {
				child: Arc::new(Mutex::new(child)),
				spawned_id,
			}),
			Box::new(CompletedTransaction),
		))
	}
}

#[derive(Debug)]
struct ProviderWrapper;

impl CommandWrapper for ProviderWrapper {
	fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
		Some(&HistoricalProvider)
	}
}

#[tokio::test]
async fn provider_historical_identity_works_without_pty_in_both_registration_orders() {
	for provider_first in [false, true] {
		let mut command = Command::new("provider-owned-program");
		if provider_first {
			command.wrap(ProviderWrapper).wrap(ProcessGroup::leader());
		} else {
			command.wrap(ProcessGroup::leader()).wrap(ProviderWrapper);
		}

		let mut child = command.spawn().expect("install process-group supervision");
		assert_eq!(child.id(), None);
		let first = child.wait().await.expect("read the cached provider status");
		assert_eq!(first.code(), Some(29));
		assert_eq!(child.wait().await.unwrap(), first);
		assert_eq!(child.try_wait().unwrap(), Some(first));
		child.signal(nix::libc::SIGCONT).unwrap();
	}
}

#[derive(Debug)]
struct StickyState {
	child: Mutex<tokio::process::Child>,
	group_signals: AtomicUsize,
}

#[derive(Debug)]
struct StickyIdChild {
	state: Arc<StickyState>,
	spawned_id: u32,
}

impl ChildWrapper for StickyIdChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn try_clone(&self) -> Option<Box<dyn ChildWrapper>> {
		Some(Box::new(Self {
			state: Arc::clone(&self.state),
			spawned_id: self.spawned_id,
		}))
	}

	fn id(&self) -> Option<u32> {
		Some(self.spawned_id)
	}

	fn spawned_id_layer(&self) -> Option<u32> {
		Some(self.spawned_id)
	}

	fn has_process_group_signal_layer(&self) -> bool {
		true
	}

	fn signal_process_group_layer(
		&mut self,
		process_group: i32,
		signal: i32,
	) -> Option<io::Result<Option<ExitStatus>>> {
		let mut child = self
			.state
			.child
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let result = ChildWrapper::signal_process_group_layer(&mut *child, process_group, signal)?;
		if matches!(result, Ok(None)) {
			self.state.group_signals.fetch_add(1, Ordering::SeqCst);
		}
		Some(result)
	}

	fn start_kill(&mut self) -> io::Result<()> {
		ChildWrapper::start_kill(
			&mut *self
				.state
				.child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner),
		)
	}

	fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
		self.state
			.child
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.try_wait()
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
		let state = Arc::clone(&self.state);
		Box::pin(std::future::poll_fn(move |context| {
			let mut child = state
				.child
				.lock()
				.unwrap_or_else(std::sync::PoisonError::into_inner);
			let mut wait = Box::pin(child.wait());
			wait.as_mut().poll(context)
		}))
	}
}

fn sticky_child(
	child: tokio::process::Child,
	publish: &Arc<Mutex<Option<Arc<StickyState>>>>,
) -> Box<dyn ChildWrapper> {
	let spawned_id = child.id().expect("the fresh child has a PID");
	let state = Arc::new(StickyState {
		child: Mutex::new(child),
		group_signals: AtomicUsize::new(0),
	});
	*publish
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&state));
	Box::new(StickyIdChild { state, spawned_id })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sticky_custom_id_does_not_authorize_a_signal_after_wait() {
	let published = Arc::new(Mutex::new(None));
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "exit 31"]);
	});
	command.wrap(ProcessGroup::leader());
	let mut child = command
		.spawn_with_child({
			let published = Arc::clone(&published);
			move |native| Ok(sticky_child(native.spawn()?, &published))
		})
		.unwrap();
	let state = published
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.take()
		.unwrap();
	let status = child.inner_mut().wait().await.unwrap();
	assert!(child.inner().id().is_some());

	child.signal(nix::libc::SIGCONT).unwrap();
	child.start_kill().unwrap();
	assert_eq!(state.group_signals.load(Ordering::SeqCst), 0);
	assert_eq!(child.wait().await.unwrap(), status);
	assert_eq!(child.try_wait().unwrap(), Some(status));
}

#[derive(Debug)]
struct SyntheticSharedState {
	status: Mutex<Option<ExitStatus>>,
	wait_acquired: Mutex<Option<mpsc::Sender<()>>>,
	completion: Mutex<Option<mpsc::Receiver<ExitStatus>>>,
	group_signals: AtomicUsize,
}

impl SyntheticSharedState {
	fn wait_under_custody(&self) -> io::Result<ExitStatus> {
		let mut status = self
			.status
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if let Some(status) = *status {
			return Ok(status);
		}
		if let Some(acquired) = self
			.wait_acquired
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
		{
			let _ = acquired.send(());
		}
		let completion = self
			.completion
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
			.ok_or_else(|| {
				io::Error::other("synthetic completion receiver was already consumed")
			})?;
		let completed = completion.recv_timeout(EXIT_TIMEOUT).map_err(|error| {
			io::Error::new(
				io::ErrorKind::TimedOut,
				format!("synthetic child completion was not released: {error}"),
			)
		})?;
		*status = Some(completed);
		Ok(completed)
	}
}

#[derive(Debug)]
struct SyntheticSharedChild {
	state: Arc<SyntheticSharedState>,
	spawned_id: u32,
}

impl ChildWrapper for SyntheticSharedChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self
	}

	fn try_clone(&self) -> Option<Box<dyn ChildWrapper>> {
		Some(Box::new(Self {
			state: Arc::clone(&self.state),
			spawned_id: self.spawned_id,
		}))
	}

	fn id(&self) -> Option<u32> {
		Some(self.spawned_id)
	}

	fn spawned_id_layer(&self) -> Option<u32> {
		Some(self.spawned_id)
	}

	fn has_process_group_signal_layer(&self) -> bool {
		true
	}

	fn signal_process_group_layer(
		&mut self,
		_process_group: i32,
		_signal: i32,
	) -> Option<io::Result<Option<ExitStatus>>> {
		let status = self
			.state
			.status
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if let Some(status) = *status {
			Some(Ok(Some(status)))
		} else {
			self.state.group_signals.fetch_add(1, Ordering::SeqCst);
			Some(Ok(None))
		}
	}

	fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
		Ok(*self
			.state
			.status
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner))
	}

	fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
		let state = Arc::clone(&self.state);
		Box::pin(async move { state.wait_under_custody() })
	}
}

struct SharedSignalCleanup {
	completion: Option<mpsc::Sender<ExitStatus>>,
	status: ExitStatus,
	workers: Vec<std::thread::JoinHandle<()>>,
}

impl SharedSignalCleanup {
	fn release(&mut self) {
		if let Some(completion) = self.completion.take() {
			let _ = completion.send(self.status);
		}
	}

	fn finish(mut self) {
		self.release();
		for worker in std::mem::take(&mut self.workers) {
			worker.join().expect("shared-child worker does not panic");
		}
	}
}

impl Drop for SharedSignalCleanup {
	fn drop(&mut self) {
		self.release();
		for worker in std::mem::take(&mut self.workers) {
			if let Err(payload) = worker.join() {
				std::mem::forget(payload);
			}
		}
	}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_wait_and_group_signal_linearize_under_child_custody() {
	let status = ExitStatus::from_raw(37 << 8);
	let (wait_acquired_tx, wait_acquired_rx) = mpsc::channel();
	let (completion_tx, completion_rx) = mpsc::channel();
	let state = Arc::new(SyntheticSharedState {
		status: Mutex::new(None),
		wait_acquired: Mutex::new(Some(wait_acquired_tx)),
		completion: Mutex::new(Some(completion_rx)),
		group_signals: AtomicUsize::new(0),
	});
	let mut command = Command::new("synthetic-child");
	command.wrap(ProcessGroup::leader());
	let mut child = command
		.spawn_with_child({
			let state = Arc::clone(&state);
			move |_| {
				Ok(Box::new(SyntheticSharedChild {
					state,
					spawned_id: 42,
				}) as Box<dyn ChildWrapper>)
			}
		})
		.unwrap();
	let mut lower = child
		.inner()
		.try_clone()
		.expect("the shared custom child exposes a clone");
	let (waited_tx, waited_rx) = mpsc::channel();
	let waiter = std::thread::spawn(move || {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.build()
			.expect("build synthetic waiter runtime");
		let result = runtime.block_on(lower.wait());
		let _ = waited_tx.send(result);
	});
	let mut cleanup = SharedSignalCleanup {
		completion: Some(completion_tx),
		status,
		workers: vec![waiter],
	};
	wait_acquired_rx
		.recv_timeout(EXIT_TIMEOUT)
		.expect("wait owns lower-child custody before signal starts");

	let (signaled_tx, signaled_rx) = mpsc::channel();
	cleanup.workers.push(std::thread::spawn(move || {
		let result = child.signal(nix::libc::SIGCONT);
		let _ = signaled_tx.send((child, result));
	}));
	let early_signal = signaled_rx.recv_timeout(Duration::from_millis(100));
	let signal_was_pending = matches!(&early_signal, Err(mpsc::RecvTimeoutError::Timeout));
	cleanup.release();
	let waited = waited_rx
		.recv_timeout(EXIT_TIMEOUT)
		.expect("wait completes after explicit terminal release")
		.expect("synthetic wait succeeds");
	let (mut child, signal_result) = match early_signal {
		Ok(result) => result,
		Err(mpsc::RecvTimeoutError::Timeout) => signaled_rx
			.recv_timeout(EXIT_TIMEOUT)
			.expect("signal completes after wait releases custody"),
		Err(mpsc::RecvTimeoutError::Disconnected) => {
			cleanup.finish();
			panic!("signal worker disconnected without a result")
		}
	};
	cleanup.finish();

	assert!(
		signal_was_pending,
		"signal did not block behind wait custody"
	);
	signal_result.unwrap();
	assert_eq!(waited, status);
	assert_eq!(state.group_signals.load(Ordering::SeqCst), 0);
	assert_eq!(child.wait().await.unwrap(), status);
	assert_eq!(child.try_wait().unwrap(), Some(status));
}
