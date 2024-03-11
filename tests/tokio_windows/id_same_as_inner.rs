use super::prelude::*;

#[tokio::test]
async fn nowrap() -> Result<()> {
	let child = TokioCommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::null());
	})
	.spawn()?;

	assert_eq!(child.id(), child.inner().id());

	Ok(())
}

#[tokio::test]
async fn job_object() -> Result<()> {
	tracing_subscriber::fmt::fmt()
		.pretty()
		.with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
		.with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
		.init();
	let child = TokioCommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::null());
	})
	.wrap(JobObject)
	.spawn()?;

	assert_eq!(child.id(), child.inner().id());

	Ok(())
}

#[tokio::test]
async fn job_object_spawn_only() -> Result<()> {
	TokioCommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::null());
	})
	.wrap(JobObject)
	.spawn()?;
	Ok(())
}
