use super::prelude::*;

#[tokio::test]
async fn nowrap() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::piped());
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
async fn process_job_object() -> Result<()> {
	let mut child = TokioCommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::piped());
	})
	.wrap(JobObject)
	.spawn()?;

	let mut output = String::new();
	if let Some(mut out) = child.stdout().take() {
		out.read_to_string(&mut output).await?;
	}

	assert_eq!(output.as_str(), "hello\n");
	Ok(())
}
