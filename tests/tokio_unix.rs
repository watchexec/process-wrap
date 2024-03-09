#![cfg(all(unix, feature = "tokio1"))]

use std::{
	io::Result,
	// os::unix::process::ExitStatusExt,
	process::Stdio,
	time::Duration,
};

use process_wrap::tokio::*;
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	time::sleep,
};

const DIE_TIME: Duration = Duration::from_millis(100);

// each test has a _nowrap variant that uses the process-wrap API but doesn't apply any Wrappers for comparison/debugging.

#[tokio::test]
async fn inner_read_stdout_nowrap() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("echo", |command| {
		command.arg("hello").stdout(Stdio::piped());
	})
	.spawn()?;

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello\n");
	Ok(())
}

#[tokio::test]
async fn inner_read_stdout_process_group() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("echo", |command| {
		command.arg("hello").stdout(Stdio::piped());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?;

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello\n");
	Ok(())
}

#[tokio::test]
async fn inner_read_stdout_process_session() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("echo", |command| {
		command.arg("hello").stdout(Stdio::piped());
	})
	.wrap(ProcessSession)
	.spawn()?;

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello\n");
	Ok(())
}

#[tokio::test]
async fn into_inner_write_stdin_nowrap() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("cat", |command| {
		command.stdin(Stdio::piped()).stdout(Stdio::piped());
	})
	.spawn()?;

	if let Some(mut din) = child.stdin().take() {
		din.write_all(b"hello").await?;
	}

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello");
	Ok(())
}

#[tokio::test]
async fn into_inner_write_stdin_process_group() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("cat", |command| {
		command.stdin(Stdio::piped()).stdout(Stdio::piped());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?
	.into_inner();

	if let Some(mut din) = child.stdin().take() {
		din.write_all(b"hello").await?;
	}

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello");
	Ok(())
}

#[tokio::test]
async fn into_inner_write_stdin_process_session() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("cat", |command| {
		command.stdin(Stdio::piped()).stdout(Stdio::piped());
	})
	.wrap(ProcessSession)
	.spawn()?
	.into_inner();

	if let Some(mut din) = child.stdin().take() {
		din.write_all(b"hello").await?;
	}

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello");
	Ok(())
}

#[tokio::test]
async fn wait_after_die_nowrap() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("echo", |command| {
		command.stdout(Stdio::null());
	})
	.spawn()?;
	sleep(DIE_TIME).await;

	let status = Box::into_pin(child.wait()).await?;
	assert!(status.success());

	Ok(())
}

#[tokio::test]
async fn wait_after_die_process_group() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("echo", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?;
	sleep(DIE_TIME).await;

	let status = Box::into_pin(child.wait()).await?;
	assert!(status.success());

	Ok(())
}

#[tokio::test]
async fn wait_after_die_process_session() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("echo", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessSession)
	.spawn()?;
	sleep(DIE_TIME).await;

	let status = Box::into_pin(child.wait()).await?;
	assert!(status.success());

	Ok(())
}

#[tokio::test]
async fn kill_and_try_wait_nowrap() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("yes", |command| {
		command.stdout(Stdio::null());
	})
	.spawn()?;
	assert!(child.try_wait()?.is_none(), "pre kill");

	Box::into_pin(child.kill()).await?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "try_wait one");

	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "try_wait two");

	Ok(())
}

#[tokio::test]
async fn kill_and_try_wait_group_process_group() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("yes", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?;
	assert!(child.try_wait()?.is_none(), "pre kill");

	Box::into_pin(child.kill()).await?;

	let status = Box::into_pin(child.wait()).await?;
	assert!(!status.success());

	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "try_wait one");

	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "try_wait two");

	Ok(())
}

#[tokio::test]
async fn kill_and_try_wait_group_process_session() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("yes", |command| {
		command.stdout(Stdio::null());
	})
	.wrap(ProcessSession)
	.spawn()?;
	assert!(child.try_wait()?.is_none(), "pre kill");

	Box::into_pin(child.kill()).await?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "try_wait one");

	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "try_wait two");

	Ok(())
}

/*
#[tokio::test]
async fn try_wait_twice_after_sigterm_nowrap() -> Result<()> {
	let mut child = Command::new("yes").stdout(Stdio::null()).spawn()?;
	assert!(child.try_wait()?.is_none(), "pre try_wait");
	child.signal(Signal::SIGTERM)?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "first try_wait");
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "second try_wait");
	Ok(())
}

#[tokio::test]
async fn try_wait_twice_after_sigterm_group() -> Result<()> {
	let mut child = Command::new("yes").stdout(Stdio::null()).group_spawn()?;
	assert!(child.try_wait()?.is_none(), "pre try_wait");
	child.signal(Signal::SIGTERM)?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "first try_wait");
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "second try_wait");
	Ok(())
}

#[tokio::test]
async fn wait_twice_after_sigterm_nowrap() -> Result<()> {
	let mut child = Command::new("yes").stdout(Stdio::null()).spawn()?;
	assert!(child.try_wait()?.is_none(), "pre try_wait");
	child.signal(Signal::SIGTERM)?;
	let status = child.wait().await?;
	assert_eq!(
		status.signal(),
		Some(Signal::SIGTERM as i32),
		"first wait status"
	);
	let status = child.wait().await?;
	assert_eq!(
		status.signal(),
		Some(Signal::SIGTERM as i32),
		"second wait status"
	);
	Ok(())
}

#[tokio::test]
async fn wait_twice_after_sigterm_group() -> Result<()> {
	let mut child = Command::new("yes").stdout(Stdio::null()).group_spawn()?;
	assert!(child.try_wait()?.is_none(), "pre try_wait");
	child.signal(Signal::SIGTERM)?;
	let status = child.wait().await?;
	assert_eq!(
		status.signal(),
		Some(Signal::SIGTERM as i32),
		"first wait status"
	);
	let status = child.wait().await?;
	assert_eq!(
		status.signal(),
		Some(Signal::SIGTERM as i32),
		"second wait status"
	);
	Ok(())
}

#[tokio::test]
async fn try_wait_after_die_nowrap() -> Result<()> {
	let mut child = Command::new("echo").stdout(Stdio::null()).spawn()?;
	sleep(DIE_TIME).await;

	let status = child.try_wait()?;
	assert!(status.is_some());
	assert!(status.unwrap().success());

	Ok(())
}

#[tokio::test]
async fn try_wait_after_die_group() -> Result<()> {
	let mut child = Command::new("echo").stdout(Stdio::null()).group_spawn()?;
	sleep(DIE_TIME).await;

	let status = child.try_wait()?;
	assert!(status.is_some());
	assert!(status.unwrap().success());

	Ok(())
}

#[tokio::test]
async fn wait_nowrap() -> Result<()> {
	let mut command = Command::new("echo");
	let mut child = command.spawn()?;
	let status = child.wait().await?;
	assert!(status.success());
	let status = child.wait().await?;
	assert!(status.success());
	Ok(())
}

#[tokio::test]
async fn wait_group() -> Result<()> {
	let mut command = Command::new("echo");
	let mut child = command.group_spawn()?;
	let status = child.wait().await?;
	assert!(status.success());
	let status = child.wait().await?;
	assert!(status.success());
	Ok(())
}

#[tokio::test]
async fn wait_with_output_nowrap() -> Result<()> {
	let child = Command::new("echo")
		.arg("hello")
		.stdout(Stdio::piped())
		.spawn()?;

	let output = child.wait_with_output().await?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[tokio::test]
async fn wait_with_output_group() -> Result<()> {
	let child = Command::new("echo")
		.arg("hello")
		.stdout(Stdio::piped())
		.group_spawn()?;

	let output = child.wait_with_output().await?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[tokio::test]
async fn id_same_as_inner_group() -> Result<()> {
	let mut command = Command::new("echo");
	let mut child = command.group_spawn()?;
	assert_eq!(child.id(), child.inner().id());
	Ok(())
}

#[tokio::test]
async fn signal_nowrap() -> Result<()> {
	let mut child = Command::new("yes").stdout(Stdio::null()).spawn()?;

	child.signal(Signal::SIGCONT)?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_none(), "not exited with sigcont");

	child.signal(Signal::SIGTERM)?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "exited with sigterm");

	Ok(())
}

#[tokio::test]
async fn signal_group() -> Result<()> {
	let mut child = Command::new("yes").stdout(Stdio::null()).group_spawn()?;

	child.signal(Signal::SIGCONT)?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_none(), "not exited with sigcont");

	child.signal(Signal::SIGTERM)?;
	sleep(DIE_TIME).await;
	assert!(child.try_wait()?.is_some(), "exited with sigterm");

	Ok(())
}
*/
