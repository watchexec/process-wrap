use super::prelude::*;

#[test]
fn nowrap() -> Result<()> {
	let child = CommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::piped());
	})
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\r\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[cfg(feature = "job-object")]
#[test]
fn job_object() -> Result<()> {
	let child = CommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::piped());
	})
	.wrap(JobObject)
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"hello\r\n".to_vec());
	assert_eq!(output.stderr, Vec::new());
	Ok(())
}

#[test]
fn stderr_only() -> Result<()> {
	let child = CommandWrap::with_new("cmd.exe", |command| {
		command
			.args(["/D", "/S", "/C", "echo hello 1>&2"])
			.stderr(Stdio::piped());
	})
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, Vec::new());
	assert_eq!(output.stderr, b"hello \r\n".to_vec());
	Ok(())
}

#[test]
fn both_streams() -> Result<()> {
	let child = CommandWrap::with_new("cmd.exe", |command| {
		command
			.args(["/D", "/S", "/C", "echo out& echo err 1>&2"])
			.stdout(Stdio::piped())
			.stderr(Stdio::piped());
	})
	.spawn()?;

	let output = (child.wait_with_output())?;
	assert!(output.status.success());
	assert_eq!(output.stdout, b"out\r\n".to_vec());
	assert_eq!(output.stderr, b"err \r\n".to_vec());
	Ok(())
}
