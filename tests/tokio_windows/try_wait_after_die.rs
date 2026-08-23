use super::prelude::*;

#[tokio::test]
async fn nowrap() -> Result<()> {
	let mut child = CommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::null());
	})
	.spawn()?;
	let mut status = None;
	for _ in 0..200 {
		status = child.try_wait()?;
		if status.is_some() {
			break;
		}
		sleep(Duration::from_millis(50)).await;
	}
	assert!(status.is_some());
	assert!(status.unwrap().success());

	Ok(())
}

#[tokio::test]
async fn job_object() -> Result<()> {
	let mut child = CommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::null());
	})
	.wrap(JobObject)
	.spawn()?;
	let mut status = None;
	for _ in 0..200 {
		status = child.try_wait()?;
		if status.is_some() {
			break;
		}
		sleep(Duration::from_millis(50)).await;
	}
	assert!(status.is_some());
	assert!(status.unwrap().success());

	Ok(())
}
