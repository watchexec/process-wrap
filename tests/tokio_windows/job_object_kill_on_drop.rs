use std::{
	fmt, fs,
	io::{Error, ErrorKind, Result},
	path::{Path, PathBuf},
	process::{Child as StdChild, Command as StdCommand, Stdio},
	time::{Duration, Instant},
};

use windows::Win32::{
	Foundation::{
		CloseHandle, HANDLE, STILL_ACTIVE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
	},
	System::Threading::{
		GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
		PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
	},
};

use super::prelude::*;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const LEAF_SELF_DEADLINE: Duration = Duration::from_secs(30);
const DIRECT_SELF_DEADLINE: Duration = Duration::from_secs(35);
const CONTROL_DIRECTORY_ENV: &str = "PROCESS_WRAP_JOB_EXTRACTION_CONTROL_DIRECTORY";
const DIRECT_HELPER: &str = "tokio_windows::job_object_kill_on_drop::descendant_parent";
const LEAF_HELPER: &str = "tokio_windows::job_object_kill_on_drop::descendant_leaf";

#[derive(Clone, Copy, Debug)]
enum Order {
	KillOnDropFirst,
	JobObjectFirst,
}

impl Order {
	fn label(self) -> &'static str {
		match self {
			Self::KillOnDropFirst => "KillOnDrop then JobObject",
			Self::JobObjectFirst => "JobObject then KillOnDrop",
		}
	}

	fn error(self, phase: &str, error: impl fmt::Display) -> Error {
		Error::other(format!("{}: {phase}: {error}", self.label()))
	}

	fn failure(self, message: impl fmt::Display) -> Error {
		Error::other(format!("{}: {message}", self.label()))
	}

	fn wrap(self, command: &mut CommandWrap) {
		match self {
			Self::KillOnDropFirst => {
				command.wrap(KillOnDrop).wrap(JobObject);
			}
			Self::JobObjectFirst => {
				command.wrap(JobObject).wrap(KillOnDrop);
			}
		}
	}
}

#[derive(Debug)]
struct ProcessGuard {
	label: &'static str,
	handle: Option<HANDLE>,
}

impl ProcessGuard {
	fn open(label: &'static str, pid: u32, order: Order) -> Result<Self> {
		// SAFETY: success returns a new process handle immediately owned by this guard.
		let handle = unsafe {
			OpenProcess(
				PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
				false,
				pid,
			)
		}
		.map_err(|error| order.error(&format!("opening {label} process {pid}"), error))?;
		Ok(Self {
			label,
			handle: Some(handle),
		})
	}

	fn handle(&self) -> HANDLE {
		self.handle
			.expect("only ProcessGuard::disarm clears the handle")
	}

	fn wait_state(&self) -> Result<WAIT_EVENT> {
		// SAFETY: this guard owns the live process handle for the nonblocking wait.
		let wait = unsafe { WaitForSingleObject(self.handle(), 0) };
		if wait == WAIT_FAILED {
			Err(Error::last_os_error())
		} else {
			Ok(wait)
		}
	}

	fn exit_code(&self) -> Result<u32> {
		let mut code = 0;
		// SAFETY: this guard owns the live process handle and `code` is writable for the call.
		unsafe { GetExitCodeProcess(self.handle(), &mut code) }.map_err(Error::other)?;
		Ok(code)
	}

	fn require_live(&self, order: Order, phase: &str) -> Result<()> {
		let wait = self.wait_state().map_err(|error| {
			order.error(&format!("querying {} during {phase}", self.label), error)
		})?;
		let code = self.exit_code().map_err(|error| {
			order.error(
				&format!("reading {} exit code during {phase}", self.label),
				error,
			)
		})?;
		if wait == WAIT_TIMEOUT && code == STILL_ACTIVE.0 as u32 {
			Ok(())
		} else {
			Err(order.failure(format!(
				"{} was not live during {phase}: wait={:#x}, exit_code={code:#x}",
				self.label, wait.0
			)))
		}
	}

	async fn wait_for_exit(&self, order: Order, phase: &str) -> Result<u32> {
		let deadline = Instant::now() + EXIT_TIMEOUT;
		loop {
			let wait = self.wait_state().map_err(|error| {
				order.error(&format!("waiting for {} during {phase}", self.label), error)
			})?;
			if wait == WAIT_OBJECT_0 {
				return self.exit_code().map_err(|error| {
					order.error(
						&format!("reading {} exit code after {phase}", self.label),
						error,
					)
				});
			}
			if wait != WAIT_TIMEOUT {
				return Err(order.failure(format!(
					"unexpected wait result {:#x} for {} during {phase}",
					wait.0, self.label
				)));
			}
			if Instant::now() >= deadline {
				let code = self.exit_code().unwrap_or(u32::MAX);
				return Err(Error::new(
					ErrorKind::TimedOut,
					format!(
						"{}: {} did not exit during {phase}; exit_code={code:#x}",
						order.label(),
						self.label
					),
				));
			}
			sleep(Duration::from_millis(10)).await;
		}
	}

	fn describe(&self) -> String {
		let wait = self.wait_state().map_or_else(
			|error| format!("error({error})"),
			|wait| format!("{:#x}", wait.0),
		);
		let code = self.exit_code().map_or_else(
			|error| format!("error({error})"),
			|code| format!("{code:#x}"),
		);
		format!("{}[wait={wait}, exit_code={code}]", self.label)
	}

	fn disarm(mut self, order: Order) -> Result<()> {
		if let Some(handle) = self.handle.take() {
			// SAFETY: taking the guard's owned handle removes the `Drop` close path.
			unsafe { CloseHandle(handle) }.map_err(|error| {
				order.error(&format!("closing {} process handle", self.label), error)
			})?;
		}
		Ok(())
	}
}

impl Drop for ProcessGuard {
	fn drop(&mut self) {
		if let Some(handle) = self.handle.take() {
			// SAFETY: the guard still owns this handle and uses its termination right only for cleanup.
			unsafe { TerminateProcess(handle, 1) }.ok();
			// SAFETY: the same handle remains live; this finite wait bounds failure cleanup.
			unsafe { WaitForSingleObject(handle, 1_000) };
			// SAFETY: the handle was taken from this sole owner and is closed exactly once.
			unsafe { CloseHandle(handle) }.ok();
		}
	}
}

#[derive(Debug)]
struct Protocol {
	directory: PathBuf,
}

impl Protocol {
	fn new(directory: &Path) -> Self {
		Self {
			directory: directory.to_owned(),
		}
	}

	fn from_environment() -> Result<Self> {
		let directory = std::env::var_os(CONTROL_DIRECTORY_ENV)
			.ok_or_else(|| Error::other("JobObject extraction control directory is missing"))?;
		Ok(Self {
			directory: directory.into(),
		})
	}

	fn pid(&self) -> PathBuf {
		self.directory.join("leaf.pid")
	}

	fn ready(&self) -> PathBuf {
		self.directory.join("leaf.ready")
	}

	fn release(&self) -> PathBuf {
		self.directory.join("leaf.release")
	}

	fn released(&self) -> PathBuf {
		self.directory.join("leaf.released")
	}

	fn deadline_failure(&self) -> PathBuf {
		self.directory.join("leaf.deadline")
	}

	fn ping_path(&self, sequence: usize) -> PathBuf {
		self.directory.join(format!("leaf.ping.{sequence}"))
	}

	fn ack(&self, sequence: usize) -> PathBuf {
		self.directory.join(format!("leaf.ack.{sequence}"))
	}

	async fn wait_for_content(
		&self,
		path: &Path,
		expected: &str,
		timeout: Duration,
		order: Order,
		phase: &str,
	) -> Result<()> {
		let deadline = Instant::now() + timeout;
		loop {
			match fs::read_to_string(path) {
				Ok(content) if content == expected => return Ok(()),
				Ok(content) if Instant::now() >= deadline => {
					return Err(Error::new(
						ErrorKind::TimedOut,
						format!(
							"{}: {phase} had unexpected marker content {content:?}; expected {expected:?}",
							order.label()
						),
					));
				}
				Err(error) if error.kind() != ErrorKind::NotFound => {
					return Err(order.error(&format!("reading {phase}"), error));
				}
				_ if Instant::now() >= deadline => {
					return Err(Error::new(
						ErrorKind::TimedOut,
						format!("{}: timed out waiting for {phase}", order.label()),
					));
				}
				_ => sleep(Duration::from_millis(10)).await,
			}
		}
	}

	async fn wait_for_pid(&self, order: Order) -> Result<u32> {
		let deadline = Instant::now() + STARTUP_TIMEOUT;
		loop {
			match fs::read_to_string(self.pid()) {
				Ok(pid) => match pid.trim().parse::<u32>() {
					Ok(pid) => return Ok(pid),
					Err(error) if Instant::now() >= deadline => {
						return Err(order.error("parsing the leaf PID marker", error));
					}
					Err(_) => {}
				},
				Err(error) if error.kind() != ErrorKind::NotFound => {
					return Err(order.error("reading the leaf PID marker", error));
				}
				Err(_) => {}
			}
			if Instant::now() >= deadline {
				return Err(Error::new(
					ErrorKind::TimedOut,
					format!("{}: leaf did not publish its PID", order.label()),
				));
			}
			sleep(Duration::from_millis(10)).await;
		}
	}

	async fn wait_until_ready(&self, pid: u32, order: Order) -> Result<()> {
		self.wait_for_content(
			&self.ready(),
			&format!("ready:{pid}"),
			STARTUP_TIMEOUT,
			order,
			"the leaf-owned ready marker",
		)
		.await
	}

	async fn ping(&self, sequence: usize, order: Order, phase: &str) -> Result<()> {
		let request = self.ping_path(sequence);
		let ack = self.ack(sequence);
		if request.exists() || ack.exists() {
			return Err(order.failure(format!(
				"stale ping state existed before {phase}: request={}, ack={}",
				request.exists(),
				ack.exists()
			)));
		}
		let token = format!("{}:{sequence}", order.label());
		publish_file(&request, &token).map_err(|error| {
			order.error(&format!("publishing ping {sequence} during {phase}"), error)
		})?;
		self.wait_for_content(
			&ack,
			&token,
			RESPONSE_TIMEOUT,
			order,
			&format!("leaf acknowledgement {sequence} during {phase}"),
		)
		.await
	}

	fn publish_release(&self, order: Order) -> Result<()> {
		publish_file(&self.release(), "release")
			.map_err(|error| order.error("publishing the leaf release", error))
	}

	fn require_released(&self, pid: u32, order: Order) -> Result<()> {
		let content = fs::read_to_string(self.released())
			.map_err(|error| order.error("reading the leaf release marker", error))?;
		if content == format!("released:{pid}") {
			Ok(())
		} else {
			Err(order.failure(format!(
				"unexpected leaf release marker {content:?} for PID {pid}"
			)))
		}
	}

	fn diagnostics(&self) -> String {
		let mut entries = Vec::new();
		for (label, path) in [
			("pid", self.pid()),
			("ready", self.ready()),
			("release", self.release()),
			("released", self.released()),
			("deadline", self.deadline_failure()),
		] {
			entries.push(marker_diagnostic(label, &path));
		}
		for sequence in 0..3 {
			entries.push(marker_diagnostic(
				&format!("ping.{sequence}"),
				&self.ping_path(sequence),
			));
			entries.push(marker_diagnostic(
				&format!("ack.{sequence}"),
				&self.ack(sequence),
			));
		}
		entries.join(", ")
	}
}

fn publish_file(path: &Path, content: &str) -> Result<()> {
	let temporary = path.with_extension("publishing");
	fs::write(&temporary, content)?;
	fs::rename(temporary, path)
}

fn marker_diagnostic(label: &str, path: &Path) -> String {
	match fs::read_to_string(path) {
		Ok(content) => format!("{label}={content:?}"),
		Err(error) if error.kind() == ErrorKind::NotFound => format!("{label}=missing"),
		Err(error) => format!("{label}=error({error})"),
	}
}

struct TreeFixture {
	_order: Order,
	_directory: tempfile::TempDir,
	protocol: Protocol,
	direct: Option<ProcessGuard>,
	descendant: Option<ProcessGuard>,
}

impl TreeFixture {
	fn new(order: Order) -> Result<Self> {
		let directory = tempfile::tempdir()
			.map_err(|error| order.error("creating control directory", error))?;
		let protocol = Protocol::new(directory.path());
		Ok(Self {
			_order: order,
			_directory: directory,
			protocol,
			direct: None,
			descendant: None,
		})
	}

	fn install_direct(&mut self, pid: u32, order: Order) -> Result<()> {
		self.direct = Some(ProcessGuard::open("direct", pid, order)?);
		Ok(())
	}

	fn install_descendant(&mut self, pid: u32, order: Order) -> Result<()> {
		self.descendant = Some(ProcessGuard::open("descendant", pid, order)?);
		Ok(())
	}

	fn direct(&self, order: Order) -> Result<&ProcessGuard> {
		self.direct
			.as_ref()
			.ok_or_else(|| order.failure("the direct-process guard was not installed"))
	}

	fn descendant(&self, order: Order) -> Result<&ProcessGuard> {
		self.descendant
			.as_ref()
			.ok_or_else(|| order.failure("the descendant-process guard was not installed"))
	}

	fn disarm_direct(&mut self, order: Order) -> Result<()> {
		self.direct
			.take()
			.ok_or_else(|| order.failure("the direct-process guard was already removed"))?
			.disarm(order)
	}

	fn disarm_descendant(&mut self, order: Order) -> Result<()> {
		self.descendant
			.take()
			.ok_or_else(|| order.failure("the descendant-process guard was already removed"))?
			.disarm(order)
	}

	fn finish(self, result: Result<()>, order: Order) -> Result<()> {
		match result {
			Ok(()) => Ok(()),
			Err(error) => {
				let process_state = [
					self.direct.as_ref().map(ProcessGuard::describe),
					self.descendant.as_ref().map(ProcessGuard::describe),
				]
				.into_iter()
				.flatten()
				.collect::<Vec<_>>()
				.join(", ");
				Err(Error::new(
					error.kind(),
					format!(
						"{error}; {} fixture state: processes=[{process_state}], markers=[{}]",
						order.label(),
						self.protocol.diagnostics()
					),
				))
			}
		}
	}
}

impl Drop for TreeFixture {
	fn drop(&mut self) {
		if self.direct.is_some() || self.descendant.is_some() {
			eprintln!(
				"{} fixture cleanup: processes=[{}], markers=[{}]",
				self._order.label(),
				[
					self.direct.as_ref().map(ProcessGuard::describe),
					self.descendant.as_ref().map(ProcessGuard::describe),
				]
				.into_iter()
				.flatten()
				.collect::<Vec<_>>()
				.join(", "),
				self.protocol.diagnostics()
			);
		}
		// Terminate the descendant first so no surviving child can outlive cleanup of the direct helper.
		drop(self.descendant.take());
		drop(self.direct.take());
	}
}

struct HelperChild(Option<StdChild>);

impl HelperChild {
	fn child_mut(&mut self) -> &mut StdChild {
		self.0
			.as_mut()
			.expect("the helper child remains owned until it exits")
	}

	fn disarm(&mut self) {
		self.0.take();
	}
}

impl Drop for HelperChild {
	fn drop(&mut self) {
		if let Some(child) = self.0.as_mut() {
			let _ = child.kill();
			let _ = child.wait();
		}
	}
}

fn direct_command(protocol: &Protocol) -> Result<CommandWrap> {
	Ok(CommandWrap::with_new(std::env::current_exe()?, |command| {
		command
			.args(["--exact", DIRECT_HELPER, "--ignored", "--nocapture"])
			.env(CONTROL_DIRECTORY_ENV, &protocol.directory)
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null());
	}))
}

async fn prepare_tree(fixture: &mut TreeFixture, order: Order) -> Result<Box<dyn ChildWrapper>> {
	let mut command = direct_command(&fixture.protocol)
		.map_err(|error| order.error("constructing the direct helper command", error))?;
	order.wrap(&mut command);
	let child = command
		.spawn()
		.map_err(|error| order.error("spawning the wrapped direct helper", error))?;
	let direct_pid = child
		.id()
		.ok_or_else(|| order.failure("the direct helper did not retain its PID"))?;
	fixture.install_direct(direct_pid, order)?;
	let descendant_pid = fixture.protocol.wait_for_pid(order).await?;
	fixture.install_descendant(descendant_pid, order)?;
	fixture
		.protocol
		.wait_until_ready(descendant_pid, order)
		.await?;
	fixture
		.direct(order)?
		.require_live(order, "pre-extraction readiness")?;
	fixture
		.descendant(order)?
		.require_live(order, "pre-extraction readiness")?;
	fixture
		.protocol
		.ping(0, order, "pre-extraction readiness")
		.await?;
	Ok(child)
}

async fn run_consuming_order_inner(fixture: &mut TreeFixture, order: Order) -> Result<()> {
	let child = prepare_tree(fixture, order).await?;
	let mut lower = child.into_inner();
	if lower
		.try_wait()
		.map_err(|error| order.error("querying the returned lower child", error))?
		.is_some()
	{
		return Err(order.failure("the returned lower child exited during extraction"));
	}
	fixture
		.direct(order)?
		.require_live(order, "post-extraction verification")?;
	fixture
		.descendant(order)?
		.require_live(order, "post-extraction verification")?;
	fixture
		.protocol
		.ping(1, order, "post-extraction verification")
		.await?;

	drop(lower);
	let direct_code = fixture
		.direct(order)?
		.wait_for_exit(order, "lower direct-child KillOnDrop")
		.await?;
	fixture.disarm_direct(order)?;
	fixture
		.descendant(order)?
		.require_live(order, "post-direct-drop verification")?;
	fixture
		.protocol
		.ping(2, order, "post-direct-drop verification")
		.await?;

	fixture.protocol.publish_release(order)?;
	let descendant_code = fixture
		.descendant(order)?
		.wait_for_exit(order, "explicit descendant release")
		.await?;
	if descendant_code != 0 {
		return Err(order.failure(format!(
			"the explicitly released descendant exited with {descendant_code:#x}; direct exit code was {direct_code:#x}"
		)));
	}
	let descendant_pid = fs::read_to_string(fixture.protocol.pid())
		.map_err(|error| order.error("re-reading the descendant PID", error))?
		.trim()
		.parse::<u32>()
		.map_err(|error| order.error("re-parsing the descendant PID", error))?;
	fixture.protocol.require_released(descendant_pid, order)?;
	fixture.disarm_descendant(order)?;
	Ok(())
}

async fn run_consuming_order(order: Order) -> Result<()> {
	let mut fixture = TreeFixture::new(order)?;
	let result = run_consuming_order_inner(&mut fixture, order).await;
	fixture.finish(result, order)
}

async fn run_installed_drop_order_inner(fixture: &mut TreeFixture, order: Order) -> Result<()> {
	let child = prepare_tree(fixture, order).await?;
	drop(child);
	fixture
		.direct(order)?
		.wait_for_exit(order, "installed JobObject drop")
		.await?;
	fixture
		.descendant(order)?
		.wait_for_exit(order, "installed JobObject drop")
		.await?;
	fixture.disarm_direct(order)?;
	fixture.disarm_descendant(order)?;
	Ok(())
}

async fn run_installed_drop_order(order: Order) -> Result<()> {
	let mut fixture = TreeFixture::new(order)?;
	let result = run_installed_drop_order_inner(&mut fixture, order).await;
	fixture.finish(result, order)
}

#[test]
#[ignore = "subprocess helper"]
fn descendant_leaf() -> Result<()> {
	let protocol = Protocol::from_environment()?;
	let pid = std::process::id();
	let deadline = Instant::now() + LEAF_SELF_DEADLINE;
	let mut next_ping = 0;

	// The controlled wait state is fully initialized before the leaf publishes its own PID/readiness.
	publish_file(&protocol.pid(), &pid.to_string())?;
	publish_file(&protocol.ready(), &format!("ready:{pid}"))?;
	loop {
		if protocol.release().exists() {
			publish_file(&protocol.released(), &format!("released:{pid}"))?;
			return Ok(());
		}
		let ping = protocol.ping_path(next_ping);
		if ping.exists() {
			let token = fs::read_to_string(&ping)?;
			publish_file(&protocol.ack(next_ping), &token)?;
			next_ping += 1;
		}
		if Instant::now() >= deadline {
			publish_file(&protocol.deadline_failure(), "leaf self-deadline expired")?;
			return Err(Error::new(
				ErrorKind::TimedOut,
				"JobObject extraction leaf exceeded its self-deadline",
			));
		}
		std::thread::sleep(Duration::from_millis(10));
	}
}

#[test]
#[ignore = "subprocess helper"]
fn descendant_parent() -> Result<()> {
	let protocol = Protocol::from_environment()?;
	let descendant = StdCommand::new(std::env::current_exe()?)
		.args(["--exact", LEAF_HELPER, "--ignored", "--nocapture"])
		.env(CONTROL_DIRECTORY_ENV, &protocol.directory)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()?;
	let mut descendant = HelperChild(Some(descendant));
	let deadline = Instant::now() + DIRECT_SELF_DEADLINE;
	loop {
		if let Some(status) = descendant.child_mut().try_wait()? {
			descendant.disarm();
			return if status.success() {
				Ok(())
			} else {
				Err(Error::other(format!(
					"JobObject extraction leaf exited with {status}"
				)))
			};
		}
		if Instant::now() >= deadline {
			return Err(Error::new(
				ErrorKind::TimedOut,
				"JobObject extraction direct helper exceeded its self-deadline",
			));
		}
		std::thread::sleep(Duration::from_millis(10));
	}
}

#[tokio::test]
async fn consuming_job_object_after_kill_on_drop_preserves_descendant() -> Result<()> {
	run_consuming_order(Order::KillOnDropFirst).await
}

#[tokio::test]
async fn consuming_job_object_before_kill_on_drop_preserves_descendant() -> Result<()> {
	run_consuming_order(Order::JobObjectFirst).await
}

#[tokio::test]
async fn installed_job_object_after_kill_on_drop_terminates_descendant() -> Result<()> {
	run_installed_drop_order(Order::KillOnDropFirst).await
}

#[tokio::test]
async fn installed_job_object_before_kill_on_drop_terminates_descendant() -> Result<()> {
	run_installed_drop_order(Order::JobObjectFirst).await
}
