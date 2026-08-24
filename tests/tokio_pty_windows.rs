#![cfg(all(windows, feature = "pty"))]

use std::{
	env,
	io::{self, Read, Write},
	path::{Path, PathBuf},
	process::ExitStatus,
	time::Duration,
};

#[cfg(feature = "creation-flags")]
use process_wrap::tokio::CreationFlags;
#[cfg(feature = "job-object")]
use process_wrap::tokio::JobObject;
#[cfg(feature = "kill-on-drop")]
use process_wrap::tokio::KillOnDrop;
use process_wrap::tokio::{
	ChildWrapper, CommandWrap, CommandWrapper, PtyCommand, PtyOptions, PtyOutput, PtySize,
};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	time::timeout,
};
use windows::Win32::{
	Foundation::HANDLE,
	System::Console::{
		ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
		ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
		STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
	},
};

#[cfg(all(feature = "creation-flags", feature = "job-object"))]
#[path = "support/windows_thread.rs"]
mod windows_thread;

const HELPER_MODE: &str = "PROCESS_WRAP_CONPTY_HELPER";
const TIMEOUT: Duration = Duration::from_secs(10);

fn helper(mode: &str) -> io::Result<PtyCommand> {
	let mut command = PtyCommand::new(env::current_exe()?);
	command
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, mode);
	Ok(command)
}

fn spawn_descendant(release: &Path) -> io::Result<std::process::Child> {
	std::process::Command::new(env::current_exe()?)
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, "descendant")
		.env("PW_RELEASE", release)
		.env_remove("PW_DESCENDANT_PID")
		.spawn()
}

struct ReleaseOnDrop(PathBuf);

impl Drop for ReleaseOnDrop {
	fn drop(&mut self) {
		let _ = std::fs::File::create(&self.0);
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
			io::stdout().write_all(b"PW-BYTES:")?;
			io::stdout().write_all(&bytes)?;
			io::stdout().flush()?;
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
			let descendant = spawn_descendant(Path::new(&release))?;
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
async fn attaches_standard_handles_before_releasing_console_ownership() -> io::Result<()> {
	let mut command = helper("terminal")?;
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let mut expected = b"PW-BYTES:".to_vec();
	expected.extend(controls);
	assert!(
		bytes
			.windows(expected.len())
			.any(|window| window == expected.as_slice()),
		"{:?}",
		bytes
	);
	Ok(())
}

#[tokio::test]
async fn reports_initial_size_and_resize() -> io::Result<()> {
	let mut command = helper("size")?;
	let initial = PtySize::new(31, 97)?;
	let (mut child, controller) = command.spawn(PtyOptions::new(initial))?;
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
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let mut command = PtyCommand::new("process-wrap-definitely-missing-program");
	assert_eq!(
		command.spawn(PtyOptions::default()).unwrap_err().kind(),
		io::ErrorKind::NotFound
	);
	command
		.program(env::current_exe()?)
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, "terminal");
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let mut command = PtyCommand::new(env::current_exe()?);
	command
		.arg("--exact")
		.raw_arg("conpty_child_helper")
		.arg("--nocapture")
		.env(HELPER_MODE, "terminal");
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
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
		let mut command = helper("terminal")?;
		if reverse {
			command
				.wrap(JobObject)
				.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP));
		} else {
			command
				.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP))
				.wrap(JobObject);
		}
		let (mut child, controller) = command.spawn(PtyOptions::default())?;
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

#[cfg(all(feature = "creation-flags", feature = "job-object"))]
#[tokio::test]
async fn job_object_preserves_explicit_suspension() -> io::Result<()> {
	use windows::Win32::System::Threading::CREATE_SUSPENDED;

	let mut command = helper("terminal")?;
	command
		.wrap(CreationFlags(CREATE_SUSPENDED))
		.wrap(JobObject);
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	let pid = child.id().expect("ConPTY children expose a process ID");
	assert!(
		windows_thread::process_has_suspended_thread(pid)?,
		"an explicitly suspended ConPTY child had no suspended thread"
	);
	child.start_kill()?;
	timeout(TIMEOUT, child.wait()).await??;
	drop(input);
	let mut bytes = Vec::new();
	timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
	assert!(
		!bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
	Ok(())
}

#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
struct ProcessGuard(Option<std::os::windows::io::OwnedHandle>);

#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
impl ProcessGuard {
	fn open(pid: u32) -> io::Result<Self> {
		use std::os::windows::io::FromRawHandle;
		use windows::Win32::System::Threading::{
			OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
		};

		// SAFETY: pid identifies the live descendant and the returned handle is non-inheritable.
		let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
			.map_err(io::Error::other)?;
		// SAFETY: OpenProcess returned a unique owned handle.
		Ok(Self(Some(unsafe {
			std::os::windows::io::OwnedHandle::from_raw_handle(process.0)
		})))
	}

	fn wait(self) -> io::Result<Self> {
		use std::os::windows::io::AsRawHandle;
		use windows::Win32::{
			Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
			System::Threading::WaitForSingleObject,
		};

		let process = self.0.as_ref().expect("a process guard must be armed");
		// SAFETY: the guard owns the live process synchronization handle.
		match unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 10_000) } {
			WAIT_OBJECT_0 => Ok(self),
			WAIT_TIMEOUT => Err(io::Error::new(
				io::ErrorKind::TimedOut,
				"ConPTY descendant survived dropping the job",
			)),
			WAIT_FAILED => Err(io::Error::last_os_error()),
			result => Err(io::Error::other(format!(
				"unexpected descendant wait result {}",
				result.0
			))),
		}
	}

	fn disarm(mut self) {
		self.0.take();
	}
}

#[cfg(all(feature = "job-object", feature = "kill-on-drop"))]
impl Drop for ProcessGuard {
	fn drop(&mut self) {
		use std::os::windows::io::AsRawHandle;
		use windows::Win32::System::Threading::{INFINITE, TerminateProcess, WaitForSingleObject};

		if let Some(process) = self.0.take() {
			let process = HANDLE(process.as_raw_handle());
			// SAFETY: the guard owns this process handle and is cleaning up a failed test descendant.
			let _ = unsafe { TerminateProcess(process, 1) };
			// SAFETY: the owned handle remains live until the end of this scope.
			let _ = unsafe { WaitForSingleObject(process, INFINITE) };
		}
	}
}

#[cfg(feature = "kill-on-drop")]
async fn assert_killed_on_drop(mut command: PtyCommand) -> io::Result<()> {
	use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
	use windows::Win32::{
		Foundation::WAIT_OBJECT_0,
		System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
	};

	let (child, controller) = command.spawn(PtyOptions::default())?;
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
		let directory = tempfile::tempdir()?;
		let release = directory.path().join("release-descendant");
		let pid_file = directory.path().join("descendant-pid");
		let _release_on_drop = ReleaseOnDrop(release.clone());
		let mut command = helper("tree")?;
		command
			.env("PW_RELEASE", &release)
			.env("PW_DESCENDANT_PID", &pid_file);
		if reverse {
			command.wrap(JobObject).wrap(KillOnDrop);
		} else {
			command.wrap(KillOnDrop).wrap(JobObject);
		}

		let (child, controller) = command.spawn(PtyOptions::default())?;
		let (input, mut output, _resize) = controller.into_parts();
		read_through(&mut output, b"PW-TREE-READY").await?;
		let pid = std::fs::read_to_string(&pid_file)?
			.parse::<u32>()
			.map_err(io::Error::other)?;
		let guard = ProcessGuard::open(pid)?;
		drop(child);
		let guard = tokio::task::spawn_blocking(move || guard.wait())
			.await
			.map_err(io::Error::other)??;
		guard.disarm();
		drop(input);
		let mut bytes = Vec::new();
		timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
	}
	Ok(())
}

#[derive(Debug)]
struct UnsupportedWrapper;

impl CommandWrapper for UnsupportedWrapper {
	fn pre_spawn(
		&mut self,
		_command: &mut tokio::process::Command,
		_core: &CommandWrap,
	) -> io::Result<()> {
		panic!("unsupported Windows PTY wrappers must be rejected before hooks")
	}
}

#[test]
fn rejects_unknown_wrappers_before_spawn_hooks() -> io::Result<()> {
	let mut command = helper("terminal")?;
	command.wrap(UnsupportedWrapper);
	let error = command.spawn(PtyOptions::default()).unwrap_err();
	assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
	assert!(error.to_string().contains("UnsupportedWrapper"));
	Ok(())
}
