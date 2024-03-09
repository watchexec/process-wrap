#![cfg(all(unix, feature = "tokio1"))]

use std::{
	io::Result,
	process::Stdio,
};

use process_wrap::tokio::*;
use tokio::{
	io::AsyncReadExt,
	process::Command,
};

// each test has a _nowrap variant that uses the process-wrap API but doesn't apply any Wrappers for comparison/debugging.

#[tokio::test]
async fn inner_read_stdout_nowrap() -> Result<()> {
	let mut command = Command::new("echo");
	command.arg("hello").stdout(Stdio::piped());
	let mut child = TokioCommandWrap::new(command).spawn()?;

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello\n");
	Ok(())
}

#[tokio::test]
async fn inner_read_stdout_process_group() -> Result<()> {
	let mut command = Command::new("echo");
	command.arg("hello").stdout(Stdio::piped());
	let mut command = TokioCommandWrap::new(command);
	command.wrap(ProcessGroup::leader());

	let mut child = command.spawn()?;
	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello\n");
	Ok(())
}
