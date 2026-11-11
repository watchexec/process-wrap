use super::prelude::*;

#[test]
fn nowrap() -> Result<()> {
	let child = CommandWrap::with_new("echo", |command| {
		command.arg("hello").stdout(Stdio::piped());
	})
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[cfg(feature = "process-group")]
#[test]
fn process_group() -> Result<()> {
	let child = CommandWrap::with_new("echo", |command| {
		command.arg("hello").stdout(Stdio::piped());
	})
	.wrap(ProcessGroup::leader())
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[cfg(feature = "process-session")]
#[test]
fn process_session() -> Result<()> {
	let child = CommandWrap::with_new("echo", |command| {
		command.arg("hello").stdout(Stdio::piped());
	})
	.wrap(ProcessSession)
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[test]
fn stderr_only() -> Result<()> {
	let child = CommandWrap::with_new("sh", |command| {
		command
			.args(["-c", "echo hello >&2"])
			.stderr(Stdio::piped());
	})
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, Vec::new());
	assert_eq!(output.stderr, b"hello\n".to_vec());
	Ok(())
}

/// More than a pipe buffer holds, so a reader waiting on one stream would leave the child blocked
/// writing to the other.
const LARGE: usize = 1 << 20;

#[test]
fn both_streams_larger_than_a_pipe() -> Result<()> {
	// All of stderr first, so the child blocks on it unless it's read while stdout is still open.
	let child = CommandWrap::with_new("sh", |command| {
		command
			.args([
				"-c",
				&format!("head -c {LARGE} /dev/zero >&2; head -c {LARGE} /dev/zero"),
			])
			.stdout(Stdio::piped())
			.stderr(Stdio::piped());
	})
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout.len(), LARGE);
	assert_eq!(output.stderr.len(), LARGE);
	Ok(())
}
