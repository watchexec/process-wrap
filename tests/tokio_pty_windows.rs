#![cfg(all(windows, feature = "pty"))]

use std::{
	env,
	io::{self, Read, Write},
	os::windows::process::CommandExt,
	panic::{AssertUnwindSafe, catch_unwind},
	path::{Path, PathBuf},
	process::{ExitStatus, Stdio},
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	time::{Duration, Instant},
};

#[cfg(feature = "creation-flags")]
use process_wrap::tokio::CreationFlags;
#[cfg(feature = "job-object")]
use process_wrap::tokio::JobObject;
#[cfg(feature = "kill-on-drop")]
use process_wrap::tokio::KillOnDrop;
use process_wrap::tokio::{
	ChildWrapper, Command, CommandWrapper, ProviderProduct, Pty, PtyController, PtyOutput, PtySize,
	SpawnAttempt, SpawnProvider,
};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	time::timeout,
};
use windows::Win32::{
	Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
	System::{
		Console::{
			ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
			ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetConsoleScreenBufferInfo,
			GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
		},
		Threading::{
			DETACHED_PROCESS, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
			TerminateProcess, WaitForSingleObject,
		},
	},
};

#[cfg(all(feature = "creation-flags", feature = "job-object"))]
#[allow(dead_code)]
#[path = "support/windows_thread.rs"]
mod windows_thread;

const HELPER_MODE: &str = "PROCESS_WRAP_CONPTY_HELPER";
const TIMEOUT: Duration = Duration::from_secs(10);

fn helper(mode: &str) -> io::Result<Command> {
	let mut command = Command::new(env::current_exe()?);
	command
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, mode);
	Ok(command)
}

fn take_controller(child: &mut dyn ChildWrapper) -> PtyController {
	let controller = child
		.take_pty_controller()
		.expect("a successful PTY spawn installs one controller");
	assert!(
		child.take_pty_controller().is_none(),
		"the PTY controller can only be extracted once"
	);
	controller
}

#[test]
fn capability_queries_agree_with_windows_backend() {
	let checked = Pty::check_supported().is_ok();
	assert_eq!(Pty::is_supported(), checked);
}

#[test]
fn capability_queries_ignore_invalid_configuration_on_windows() {
	let invalid = Pty::new(PtySize {
		rows: 0,
		columns: 0,
		pixel_width: 0,
		pixel_height: 0,
	});
	let checked = Pty::check_supported().is_ok();

	assert_eq!(invalid.check_available().is_ok(), checked);
}

fn spawn_with_terminal(
	command: &mut Command,
	size: PtySize,
) -> io::Result<(Box<dyn ChildWrapper>, PtyController)> {
	command.wrap(Pty::new(size));
	let mut child = command.spawn()?;
	let controller = take_controller(child.as_mut());
	Ok((child, controller))
}

fn descendant_command(release: &Path) -> io::Result<std::process::Command> {
	let mut command = std::process::Command::new(env::current_exe()?);
	command
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, "descendant")
		.env("PW_RELEASE", release)
		.env_remove("PW_DESCENDANT_PID");
	Ok(command)
}

fn spawn_descendant(release: &Path) -> io::Result<std::process::Child> {
	descendant_command(release)?.spawn()
}

fn spawn_detached_descendant(release: &Path) -> io::Result<std::process::Child> {
	descendant_command(release)?
		.creation_flags(DETACHED_PROCESS.0)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()
}

struct ReleaseOnDrop(PathBuf);

impl Drop for ReleaseOnDrop {
	fn drop(&mut self) {
		let _ = std::fs::File::create(&self.0);
	}
}

#[derive(Debug)]
struct ProcessExitGuard(Option<HANDLE>);

// SAFETY: the guard uniquely owns a process-wide Windows handle. Moving it to another thread
// transfers that ownership without exposing Rust memory or retaining an alias which could close it.
unsafe impl Send for ProcessExitGuard {}

impl ProcessExitGuard {
	fn open(pid: u32) -> io::Result<Self> {
		// SAFETY: the access mask is valid for process handles, the numeric PID carries no borrowed
		// memory, and a successful call returns a new handle owned by this guard.
		let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(io::Error::other)?;
		Ok(Self(Some(handle)))
	}

	fn has_exited(&self) -> io::Result<bool> {
		// SAFETY: the guard still owns this live process handle for the duration of the nonblocking wait.
		let wait = unsafe { WaitForSingleObject(self.handle(), 0) };
		if wait == WAIT_FAILED {
			Err(io::Error::last_os_error())
		} else {
			Ok(wait == WAIT_OBJECT_0)
		}
	}

	fn disarm(mut self) -> io::Result<()> {
		if let Some(handle) = self.0.take() {
			// SAFETY: taking the handle removes the guard's only owner, so this closes it exactly once.
			unsafe { CloseHandle(handle) }?;
		}
		Ok(())
	}

	fn handle(&self) -> HANDLE {
		self.0
			.expect("only ProcessExitGuard::disarm clears the handle, and it consumes the guard")
	}
}

impl Drop for ProcessExitGuard {
	fn drop(&mut self) {
		if let Some(handle) = self.0.take() {
			// SAFETY: taking the handle removes the guard's only owner. It remains live until the final
			// CloseHandle below, and all three operations use it only as a process handle.
			unsafe { TerminateProcess(handle, 1) }.ok();
			// SAFETY: the uniquely owned process handle remains live until the following close.
			unsafe { WaitForSingleObject(handle, INFINITE) };
			// SAFETY: this is the sole remaining owner and no subsequent operation uses the handle.
			unsafe { CloseHandle(handle) }.ok();
		}
	}
}

async fn wait_for_process_exit(guard: ProcessExitGuard) -> io::Result<()> {
	let deadline = Instant::now() + TIMEOUT;
	loop {
		if guard.has_exited()? {
			return guard.disarm();
		}
		if Instant::now() >= deadline {
			return Err(io::Error::new(
				io::ErrorKind::TimedOut,
				"ConPTY process survived the failed spawn lifecycle",
			));
		}
		tokio::time::sleep(Duration::from_millis(10)).await;
	}
}

async fn read_through(output: &mut PtyOutput, needle: &[u8]) -> io::Result<Vec<u8>> {
	timeout(TIMEOUT, async {
		let mut bytes = Vec::new();
		let mut buffer = [0; 1024];
		while !bytes.windows(needle.len()).any(|window| window == needle) {
			let read = output.read(&mut buffer).await?;
			if read == 0 {
				return Err(io::Error::new(
					io::ErrorKind::UnexpectedEof,
					"ConPTY output closed before the expected marker",
				));
			}
			bytes.extend_from_slice(&buffer[..read]);
		}
		Ok(bytes)
	})
	.await
	.map_err(io::Error::other)?
}

async fn wait_and_drain(
	child: &mut dyn ChildWrapper,
	output: &mut PtyOutput,
	bytes: &mut Vec<u8>,
) -> io::Result<ExitStatus> {
	let (status, _) = timeout(TIMEOUT, async {
		tokio::try_join!(child.wait(), output.read_to_end(bytes))
	})
	.await
	.map_err(io::Error::other)??;
	Ok(status)
}

fn terminal_handles() -> io::Result<(HANDLE, HANDLE, HANDLE)> {
	// SAFETY: these calls only retrieve this process's standard handles.
	unsafe {
		Ok((
			GetStdHandle(STD_INPUT_HANDLE).map_err(io::Error::other)?,
			GetStdHandle(STD_OUTPUT_HANDLE).map_err(io::Error::other)?,
			GetStdHandle(STD_ERROR_HANDLE).map_err(io::Error::other)?,
		))
	}
}

fn is_console(handle: HANDLE) -> bool {
	let mut mode = Default::default();
	// SAFETY: handle is borrowed from a live standard handle and mode is writable storage.
	unsafe { GetConsoleMode(handle, &mut mode) }.is_ok()
}

#[test]
fn conpty_child_helper() -> io::Result<()> {
	let Ok(mode) = env::var(HELPER_MODE) else {
		return Ok(());
	};
	let (stdin_handle, stdout_handle, stderr_handle) = terminal_handles()?;

	match mode.as_str() {
		"terminal" => {
			print!(
				"PW-TERMINALS:{}{}{}:{}:REMOVED={}:CWD={}",
				is_console(stdin_handle) as u8,
				is_console(stdout_handle) as u8,
				is_console(stderr_handle) as u8,
				env::var("PW_VALUE").unwrap_or_default(),
				env::var_os("PW_REMOVED").is_none() as u8,
				env::current_dir()?.display(),
			);
			eprint!("PW-STDERR");
			io::stdout().flush()?;
			io::stderr().flush()?;
		}
		"io" => {
			let mut console_mode = Default::default();
			// SAFETY: stdin_handle is the live console input handle and console_mode is writable.
			unsafe { GetConsoleMode(stdin_handle, &mut console_mode) }.map_err(io::Error::other)?;
			console_mode &= !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
			console_mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
			// SAFETY: stdin_handle remains live and console_mode is a valid combination of input flags.
			unsafe { SetConsoleMode(stdin_handle, console_mode) }.map_err(io::Error::other)?;
			print!("PW-READY");
			io::stdout().flush()?;
			let mut bytes = [0; 4];
			io::stdin().read_exact(&mut bytes)?;
			let mut stdout = io::stdout();
			stdout.write_all(b"PW-BYTES:")?;
			for byte in bytes {
				write!(stdout, "{byte:02x}")?;
			}
			stdout.flush()?;
		}
		"size" => {
			print_size(stdout_handle)?;
			io::stdout().flush()?;
			let mut line = String::new();
			io::stdin().read_line(&mut line)?;
			print_size(stdout_handle)?;
			io::stdout().flush()?;
		}
		"descendant-parent" => {
			let release = env::var_os("PW_RELEASE").ok_or_else(|| {
				io::Error::new(io::ErrorKind::InvalidInput, "PW_RELEASE is unset")
			})?;
			drop(spawn_descendant(Path::new(&release))?);
		}
		"descendant" => {
			let release = env::var_os("PW_RELEASE").ok_or_else(|| {
				io::Error::new(io::ErrorKind::InvalidInput, "PW_RELEASE is unset")
			})?;
			print!("PW-DESCENDANT-READY");
			io::stdout().flush()?;
			while !Path::new(&release).exists() {
				std::thread::sleep(Duration::from_millis(10));
			}
		}
		"tree" => {
			let release = env::var_os("PW_RELEASE").ok_or_else(|| {
				io::Error::new(io::ErrorKind::InvalidInput, "PW_RELEASE is unset")
			})?;
			let pid_file = env::var_os("PW_DESCENDANT_PID").ok_or_else(|| {
				io::Error::new(io::ErrorKind::InvalidInput, "PW_DESCENDANT_PID is unset")
			})?;
			let descendant = spawn_detached_descendant(Path::new(&release))?;
			std::fs::write(pid_file, descendant.id().to_string())?;
			drop(descendant);
			print!("PW-TREE-READY");
			io::stdout().flush()?;
			let mut byte = [0];
			loop {
				io::stdin().read_exact(&mut byte)?;
			}
		}
		"wait" => {
			print!("PW-READY");
			io::stdout().flush()?;
			let mut byte = [0];
			loop {
				io::stdin().read_exact(&mut byte)?;
			}
		}
		other => panic!("unknown ConPTY helper mode {other}"),
	}
	Ok(())
}

fn print_size(output: HANDLE) -> io::Result<()> {
	let mut info = Default::default();
	// SAFETY: output is a live console output handle and info is writable output storage.
	unsafe { GetConsoleScreenBufferInfo(output, &mut info) }.map_err(io::Error::other)?;
	print!("PW-SIZE:{}x{}", info.dwSize.Y, info.dwSize.X);
	Ok(())
}

#[tokio::test]
async fn spawns_with_three_console_standard_handles() -> io::Result<()> {
	let mut command = helper("terminal")?;
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);

	let mut bytes = read_through(&mut output, b"PW-TERMINALS:111").await?;
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	Ok(())
}

#[tokio::test]
async fn exposes_terminal_handles_and_preserves_environment_cwd_and_merged_output() -> io::Result<()>
{
	let directory = tempfile::tempdir()?;
	let system_root = env::var_os("SystemRoot").expect("Windows must define SystemRoot");
	let mut command = helper("terminal")?;
	command
		.env("PW_DISCARDED", "before-clear")
		.env_clear()
		.env("SystemRoot", system_root)
		.env(HELPER_MODE, "terminal")
		.env("PW_VALUE", "child-value")
		.env("PW_REMOVED", "discarded")
		.env_remove("PW_REMOVED")
		.current_dir(directory.path());

	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	let marker = format!(
		"PW-TERMINALS:111:child-value:REMOVED=1:CWD={}",
		directory.path().display()
	);
	assert!(
		bytes
			.windows(marker.len())
			.any(|window| window == marker.as_bytes()),
		"{}",
		String::from_utf8_lossy(&bytes)
	);
	assert!(
		bytes
			.windows(b"PW-STDERR".len())
			.any(|window| window == b"PW-STDERR"),
		"{}",
		String::from_utf8_lossy(&bytes)
	);
	Ok(())
}

#[tokio::test]
async fn passes_bidirectional_terminal_bytes() -> io::Result<()> {
	let mut command = helper("io")?;
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (mut input, mut output, _resize) = controller.into_parts();
	let mut bytes = read_through(&mut output, b"PW-READY").await?;

	let controls = [b'A', 0x1b, b'B', b'C'];
	input.write_all(&controls).await?;
	input.shutdown().await?;
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	let expected = b"PW-BYTES:411b4243";
	assert!(
		bytes
			.windows(expected.len())
			.any(|window| window == expected),
		"{:?}",
		bytes
	);
	Ok(())
}

#[tokio::test]
async fn reports_initial_size_and_resize() -> io::Result<()> {
	let mut command = helper("size")?;
	let initial = PtySize::new(31, 97)?;
	let (mut child, controller) = spawn_with_terminal(&mut command, initial)?;
	let (mut input, mut output, resize) = controller.into_parts();
	read_through(&mut output, b"PW-SIZE:31x97").await?;
	resize.resize(PtySize::new(42, 113)?)?;
	input.write_all(b"go\r").await?;
	read_through(&mut output, b"PW-SIZE:42x113").await?;
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	Ok(())
}

#[tokio::test]
async fn canceled_wait_kill_and_repeated_waits_remain_valid() -> io::Result<()> {
	let mut command = helper("wait")?;
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	read_through(&mut output, b"PW-READY").await?;
	assert!(
		timeout(Duration::from_millis(20), child.wait())
			.await
			.is_err()
	);
	child.start_kill()?;
	let status = timeout(TIMEOUT, child.wait()).await??;
	assert_eq!(child.try_wait()?, Some(status));
	assert_eq!(child.wait().await?, status);
	drop(input);
	let mut trailing = Vec::new();
	timeout(TIMEOUT, output.read_to_end(&mut trailing)).await??;
	Ok(())
}

#[tokio::test]
async fn shutting_down_input_releases_its_owner_without_early_hangup() -> io::Result<()> {
	let mut command = helper("wait")?;
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (mut input, mut output, resize) = controller.into_parts();
	read_through(&mut output, b"PW-READY").await?;

	input.shutdown().await?;
	assert_eq!(
		input.write_all(b"closed").await.unwrap_err().kind(),
		io::ErrorKind::BrokenPipe
	);
	assert!(child.try_wait()?.is_none());
	resize.resize(PtySize::default())?;
	drop(output);
	assert_eq!(
		resize.resize(PtySize::default()).unwrap_err().kind(),
		io::ErrorKind::BrokenPipe
	);
	timeout(TIMEOUT, child.wait()).await??;
	Ok(())
}

#[tokio::test]
async fn dropping_output_keeps_the_terminal_live_while_input_exists() -> io::Result<()> {
	let mut command = helper("wait")?;
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, resize) = controller.into_parts();
	read_through(&mut output, b"PW-READY").await?;
	drop(output);
	assert!(child.try_wait()?.is_none());
	resize.resize(PtySize::default())?;
	drop(input);
	assert_eq!(
		resize.resize(PtySize::default()).unwrap_err().kind(),
		io::ErrorKind::BrokenPipe
	);
	timeout(TIMEOUT, child.wait()).await??;
	Ok(())
}

#[tokio::test]
async fn direct_child_wait_is_independent_from_descendant_output_eof() -> io::Result<()> {
	let directory = tempfile::tempdir()?;
	let release = directory.path().join("release-descendant");
	let _release_on_drop = ReleaseOnDrop(release.clone());
	let mut command = helper("descendant-parent")?;
	command.env("PW_RELEASE", &release);

	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = read_through(&mut output, b"PW-DESCENDANT-READY").await?;
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	assert!(
		timeout(Duration::from_millis(100), output.read_to_end(&mut bytes))
			.await
			.is_err(),
		"ConPTY output reached EOF while a descendant remained attached"
	);
	std::fs::File::create(&release)?;
	timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
	Ok(())
}

#[tokio::test]
async fn failed_spawn_leaves_the_tracked_command_reusable() -> io::Result<()> {
	let directory = tempfile::tempdir()?;
	let executable = directory.path().join("retry-helper.exe");
	let mut command = Command::new(&executable);
	command
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, "terminal")
		.wrap(Pty::default());
	let error = command
		.spawn()
		.err()
		.expect("the first spawn must fail while the executable is absent");
	assert_eq!(error.kind(), io::ErrorKind::NotFound);

	std::fs::copy(env::current_exe()?, &executable)?;
	let mut child = command.spawn()?;
	let controller = take_controller(child.as_mut());
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}

#[tokio::test]
async fn accepts_ordered_raw_argument_fragments() -> io::Result<()> {
	let mut command = Command::new(env::current_exe()?);
	command
		.arg("--exact")
		.raw_arg("\"conpty_child_helper\"")
		.arg("--nocapture")
		.env(HELPER_MODE, "terminal");
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}

#[cfg(feature = "creation-flags")]
#[tokio::test]
async fn creation_flags_compose_without_a_job_object() -> io::Result<()> {
	use windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

	let mut command = helper("terminal")?;
	command.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP));
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}

#[cfg(feature = "job-object")]
#[tokio::test]
async fn job_object_resumes_its_temporarily_suspended_primary_thread() -> io::Result<()> {
	let mut command = helper("terminal")?;
	command.wrap(JobObject);
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}

#[cfg(all(feature = "creation-flags", feature = "job-object"))]
#[tokio::test]
async fn job_object_composes_with_creation_flags_in_both_orders() -> io::Result<()> {
	use windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

	for reverse in [false, true] {
		for pty_first in [false, true] {
			let mut command = helper("terminal")?;
			if pty_first {
				command.wrap(Pty::default());
			}
			if reverse {
				command
					.wrap(JobObject)
					.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP));
			} else {
				command
					.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP))
					.wrap(JobObject);
			}
			if !pty_first {
				command.wrap(Pty::default());
			}
			let mut child = command.spawn()?;
			let controller = take_controller(child.as_mut());
			let (input, mut output, _resize) = controller.into_parts();
			drop(input);
			let mut bytes = Vec::new();
			assert!(
				wait_and_drain(child.as_mut(), &mut output, &mut bytes)
					.await?
					.success()
			);
			assert!(
				bytes
					.windows(16)
					.any(|window| window == b"PW-TERMINALS:111")
			);
		}
	}
	Ok(())
}

#[cfg(all(feature = "creation-flags", feature = "job-object"))]
#[tokio::test]
async fn job_object_preserves_explicit_suspension() -> io::Result<()> {
	use windows::Win32::System::Threading::CREATE_SUSPENDED;

	let mut command = helper("terminal")?;
	command
		.wrap(CreationFlags(CREATE_SUSPENDED))
		.wrap(JobObject);
	let (mut child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	let pid = child.id().expect("ConPTY children expose a process ID");
	assert!(
		windows_thread::process_has_suspended_thread(pid)?,
		"an explicitly suspended ConPTY child had no suspended thread"
	);
	windows_thread::resume_process_threads(pid)?;
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}

#[cfg(feature = "kill-on-drop")]
async fn assert_killed_on_drop(mut command: Command) -> io::Result<()> {
	use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
	use windows::Win32::{
		Foundation::WAIT_OBJECT_0,
		System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
	};

	let (child, controller) = spawn_with_terminal(&mut command, PtySize::default())?;
	let pid = child.id().expect("ConPTY children expose a process ID");
	let (_input, mut output, _resize) = controller.into_parts();
	read_through(&mut output, b"PW-READY").await?;
	// SAFETY: pid identifies the live child. Ownership of the non-inheritable handle is transferred.
	let process =
		unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.map_err(io::Error::other)?;
	// SAFETY: OpenProcess returned a unique owned handle.
	let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
	drop(child);
	let wait = tokio::task::spawn_blocking(move || {
		// SAFETY: process owns the live synchronization handle for this wait.
		unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 10_000) }
	});
	assert_eq!(wait.await.map_err(io::Error::other)?, WAIT_OBJECT_0);
	Ok(())
}

#[cfg(feature = "kill-on-drop")]
#[tokio::test]
async fn kill_on_drop_terminates_a_conpty_child() -> io::Result<()> {
	let mut command = helper("wait")?;
	command.wrap(KillOnDrop);
	assert_killed_on_drop(command).await
}

#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
#[tokio::test]
async fn job_object_composes_with_kill_on_drop_in_both_orders() -> io::Result<()> {
	for reverse in [false, true] {
		for pty_first in [false, true] {
			let directory = tempfile::tempdir()?;
			let release = directory.path().join("release-descendant");
			let pid_file = directory.path().join("descendant-pid");
			let _release_on_drop = ReleaseOnDrop(release.clone());
			let mut command = helper("tree")?;
			command
				.env("PW_RELEASE", &release)
				.env("PW_DESCENDANT_PID", &pid_file);
			if pty_first {
				command.wrap(Pty::default());
			}
			if reverse {
				command.wrap(JobObject).wrap(KillOnDrop);
			} else {
				command.wrap(KillOnDrop).wrap(JobObject);
			}
			if !pty_first {
				command.wrap(Pty::default());
			}

			let mut child = command.spawn()?;
			let controller = take_controller(child.as_mut());
			let (input, mut output, _resize) = controller.into_parts();
			read_through(&mut output, b"PW-TREE-READY").await?;
			let pid = std::fs::read_to_string(&pid_file)?
				.parse::<u32>()
				.map_err(io::Error::other)?;
			let guard = ProcessExitGuard::open(pid)?;
			drop(child);
			wait_for_process_exit(guard).await?;
			drop(input);
			let mut bytes = Vec::new();
			timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
		}
	}
	Ok(())
}

#[derive(Clone, Copy, Debug)]
enum LifecycleFailure {
	Error,
	Panic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleStage {
	PostSpawn,
	WrapChild,
	FinalizeSpawn,
	DisarmSpawnCleanup,
	DisarmJobObject,
}

fn fail_lifecycle<T>(failure: LifecycleFailure) -> io::Result<T> {
	match failure {
		LifecycleFailure::Error => Err(io::Error::other("ConPTY lifecycle failed")),
		LifecycleFailure::Panic => panic!("ConPTY lifecycle failed"),
	}
}

#[derive(Debug)]
struct FailFinalizationChild {
	inner: Box<dyn ChildWrapper>,
	failure: LifecycleFailure,
	stage: LifecycleStage,
}

impl ChildWrapper for FailFinalizationChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.inner.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.inner.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.inner
	}

	fn finalize_spawn_layer(&mut self) -> io::Result<()> {
		if self.stage == LifecycleStage::FinalizeSpawn {
			fail_lifecycle(self.failure)
		} else {
			Ok(())
		}
	}

	fn disarm_spawn_cleanup_layer(&mut self) -> io::Result<()> {
		if self.stage == LifecycleStage::DisarmSpawnCleanup {
			fail_lifecycle(self.failure)
		} else {
			Ok(())
		}
	}

	fn disarm_job_object_layer(&mut self) -> io::Result<()> {
		if self.stage == LifecycleStage::DisarmJobObject {
			fail_lifecycle(self.failure)
		} else {
			Ok(())
		}
	}
}

#[derive(Debug)]
struct FailLifecycleOnce {
	failure: LifecycleFailure,
	stage: LifecycleStage,
	failed: bool,
	process: Arc<Mutex<Option<ProcessExitGuard>>>,
}

impl FailLifecycleOnce {
	fn capture(&self, child: &mut dyn ChildWrapper) -> io::Result<()> {
		assert!(child.take_pty_controller().is_none());
		let pid = child
			.id()
			.expect("a newly spawned ConPTY child exposes its process ID");
		*self.process.lock().unwrap() = Some(ProcessExitGuard::open(pid)?);
		Ok(())
	}
}

impl CommandWrapper for FailLifecycleOnce {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		if self.stage != LifecycleStage::PostSpawn || self.failed {
			return Ok(());
		}
		self.capture(child)?;
		self.failed = true;
		fail_lifecycle(self.failure)
	}

	fn wrap_child(
		&mut self,
		mut child: Box<dyn ChildWrapper>,
		_command: &Command,
	) -> io::Result<Box<dyn ChildWrapper>> {
		if self.failed || self.stage == LifecycleStage::PostSpawn {
			return Ok(child);
		}
		self.capture(child.as_mut())?;
		self.failed = true;
		if self.stage == LifecycleStage::WrapChild {
			fail_lifecycle(self.failure)
		} else {
			Ok(Box::new(FailFinalizationChild {
				inner: child,
				failure: self.failure,
				stage: self.stage,
			}))
		}
	}
}

#[tokio::test]
async fn failed_windows_lifecycle_reaps_the_child_and_remains_reusable() -> io::Result<()> {
	for stage in [
		LifecycleStage::PostSpawn,
		LifecycleStage::WrapChild,
		LifecycleStage::FinalizeSpawn,
		LifecycleStage::DisarmSpawnCleanup,
		LifecycleStage::DisarmJobObject,
	] {
		for failure in [LifecycleFailure::Error, LifecycleFailure::Panic] {
			let process = Arc::new(Mutex::new(None));
			let mut command = helper("wait")?;
			command.wrap(Pty::default()).wrap(FailLifecycleOnce {
				failure,
				stage,
				failed: false,
				process: Arc::clone(&process),
			});

			let result = catch_unwind(AssertUnwindSafe(|| command.spawn()));
			match failure {
				LifecycleFailure::Error => assert_eq!(
					result.unwrap().unwrap_err().to_string(),
					"ConPTY lifecycle failed"
				),
				LifecycleFailure::Panic => assert_eq!(
					*result
						.expect_err("the Windows lifecycle hook must panic")
						.downcast::<&'static str>()
						.unwrap(),
					"ConPTY lifecycle failed"
				),
			}
			let guard = process
				.lock()
				.unwrap()
				.take()
				.expect("the failing lifecycle hook captures the child process");
			wait_for_process_exit(guard).await?;

			let mut child = command.spawn()?;
			let controller = take_controller(child.as_mut());
			let (input, mut output, _resize) = controller.into_parts();
			read_through(&mut output, b"PW-READY").await?;
			child.start_kill()?;
			timeout(TIMEOUT, child.wait()).await??;
			drop(input);
			timeout(TIMEOUT, output.read_to_end(&mut Vec::new())).await??;
		}
	}
	Ok(())
}

#[derive(Debug)]
struct OtherProvider;

impl SpawnProvider for OtherProvider {
	fn spawn(
		&self,
		_attempt: &mut SpawnAttempt,
		_command: &Command,
	) -> io::Result<ProviderProduct> {
		panic!("provider conflicts are rejected before spawning")
	}
}

impl CommandWrapper for OtherProvider {
	fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
		Some(self)
	}
}

#[test]
fn rejects_another_spawn_provider_before_callbacks() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::default()).wrap(OtherProvider);
	let error = command.spawn().unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert_eq!(error.to_string(), "multiple spawn providers are registered");
}

#[derive(Debug)]
struct MakeAttemptNative;

impl CommandWrapper for MakeAttemptNative {
	fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<()> {
		let _ = attempt.native_mut();
		Ok(())
	}
}

#[test]
fn rejects_opaque_base_and_attempt_state() {
	let native = tokio::process::Command::new("ignored");
	let mut command = Command::from(native);
	command.wrap(Pty::default());
	let error = command.spawn().unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert_eq!(
		error.to_string(),
		"a spawn provider cannot use a native-only command"
	);

	let mut command = Command::new("ignored");
	command.wrap(Pty::default()).wrap(MakeAttemptNative);
	let error = command.spawn().unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert_eq!(
		error.to_string(),
		"a spawn provider cannot use a native-only spawn attempt"
	);
}

#[test]
fn explicit_spawners_cannot_bypass_pty() {
	let mut command = Command::new("ignored");
	command.wrap(Pty::default());
	let error = command
		.spawn_with(|_| panic!("explicit spawner must not run"))
		.unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

	let error = command
		.spawn_with_child(|_| panic!("explicit child spawner must not run"))
		.unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn duplicate_pty_registration_uses_the_later_size() -> io::Result<()> {
	let mut command = helper("size")?;
	command
		.wrap(Pty::new(PtySize::new(31, 97)?))
		.wrap(Pty::new(PtySize::new(42, 113)?));

	let mut child = command.spawn()?;
	let controller = take_controller(child.as_mut());
	let (mut input, mut output, _resize) = controller.into_parts();
	read_through(&mut output, b"PW-SIZE:42x113").await?;
	input.write_all(b"go\r").await?;
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	Ok(())
}

#[tokio::test]
async fn provider_and_controller_are_reusable() -> io::Result<()> {
	let mut command = helper("terminal")?;
	command.wrap(Pty::default());

	for _ in 0..3 {
		let mut child = command.spawn()?;
		let controller = take_controller(child.as_mut());
		let (input, mut output, _resize) = controller.into_parts();
		drop(input);
		let mut bytes = Vec::new();
		assert!(
			wait_and_drain(child.as_mut(), &mut output, &mut bytes)
				.await?
				.success()
		);
		assert!(
			bytes
				.windows(16)
				.any(|window| window == b"PW-TERMINALS:111")
		);
	}
	Ok(())
}

#[tokio::test]
async fn non_pty_children_have_no_controller() -> io::Result<()> {
	let mut command = Command::with_new("cmd.exe", |command| {
		command.args(["/D", "/S", "/C", "exit /b 0"]);
	});
	let mut child = command.spawn()?;
	assert!(child.take_pty_controller().is_none());
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	Ok(())
}

#[derive(Debug)]
struct TransparentChild(Box<dyn ChildWrapper>);

impl ChildWrapper for TransparentChild {
	fn inner(&self) -> &dyn ChildWrapper {
		self.0.as_ref()
	}

	fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
		self.0.as_mut()
	}

	fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
		self.0
	}
}

#[derive(Debug)]
struct ObserveControllerLifecycle {
	post_spawn: Arc<AtomicBool>,
	wrap_child: Arc<AtomicBool>,
}

impl CommandWrapper for ObserveControllerLifecycle {
	fn post_spawn(
		&mut self,
		_attempt: &mut SpawnAttempt,
		child: &mut dyn ChildWrapper,
		_command: &Command,
	) -> io::Result<()> {
		assert!(child.take_pty_controller().is_none());
		self.post_spawn.store(true, Ordering::SeqCst);
		Ok(())
	}

	fn wrap_child(
		&mut self,
		mut child: Box<dyn ChildWrapper>,
		_command: &Command,
	) -> io::Result<Box<dyn ChildWrapper>> {
		assert!(child.take_pty_controller().is_none());
		self.wrap_child.store(true, Ordering::SeqCst);
		Ok(Box::new(TransparentChild(child)))
	}
}

#[tokio::test]
async fn controller_commits_after_hooks_and_traverses_outer_wrappers() -> io::Result<()> {
	let post_spawn = Arc::new(AtomicBool::new(false));
	let wrap_child = Arc::new(AtomicBool::new(false));
	let mut command = helper("terminal")?;
	command
		.wrap(Pty::default())
		.wrap(ObserveControllerLifecycle {
			post_spawn: Arc::clone(&post_spawn),
			wrap_child: Arc::clone(&wrap_child),
		});

	let mut child = command.spawn()?;
	assert!(post_spawn.load(Ordering::SeqCst));
	assert!(wrap_child.load(Ordering::SeqCst));
	let controller = take_controller(child.as_mut());
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	Ok(())
}

#[derive(Debug)]
struct PortableWrapper(Arc<AtomicBool>);

impl CommandWrapper for PortableWrapper {
	fn pre_spawn(&mut self, _attempt: &mut SpawnAttempt, _command: &Command) -> io::Result<()> {
		self.0.store(true, Ordering::SeqCst);
		Ok(())
	}
}

#[tokio::test]
async fn portable_custom_wrappers_compose_with_conpty() -> io::Result<()> {
	let called = Arc::new(AtomicBool::new(false));
	let mut command = helper("terminal")?;
	command
		.wrap(Pty::default())
		.wrap(PortableWrapper(Arc::clone(&called)));

	let mut child = command.spawn()?;
	assert!(called.load(Ordering::SeqCst));
	assert!(child.stdin().is_none());
	assert!(child.stdout().is_none());
	assert!(child.stderr().is_none());
	assert!(child.try_inner_child().is_none());
	let controller = take_controller(child.as_mut());
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut bytes = Vec::new();
	assert!(
		wait_and_drain(child.as_mut(), &mut output, &mut bytes)
			.await?
			.success()
	);
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}
