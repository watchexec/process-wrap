#![cfg(all(windows, feature = "pty"))]

use std::{
	env,
	io::{self, Read, Write},
	time::Duration,
};

use process_wrap::tokio::{CommandWrap, CommandWrapper, PtyCommand, PtyOptions, PtySize};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	time::timeout,
};
use windows::Win32::{
	Foundation::HANDLE,
	System::Console::{
		ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode,
		GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
		STD_OUTPUT_HANDLE, SetConsoleMode,
	},
};

const HELPER_MODE: &str = "PROCESS_WRAP_CONPTY_HELPER";
const TIMEOUT: Duration = Duration::from_secs(10);

fn helper(mode: &str) -> io::Result<PtyCommand> {
	let mut command = PtyCommand::new(env::current_exe()?);
	command
		.args(["--exact", "conpty_child_helper", "--nocapture"])
		.env(HELPER_MODE, mode);
	Ok(command)
}

async fn read_through(
	output: &mut process_wrap::tokio::PtyOutput,
	needle: &[u8],
) -> io::Result<Vec<u8>> {
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
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	let mut bytes = Vec::new();
	timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
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
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
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
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	let mut bytes = Vec::new();
	timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
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
	assert!(timeout(TIMEOUT, child.wait()).await??.success());
	let mut bytes = Vec::new();
	timeout(TIMEOUT, output.read_to_end(&mut bytes)).await??;
	assert!(
		bytes
			.windows(16)
			.any(|window| window == b"PW-TERMINALS:111")
	);
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
