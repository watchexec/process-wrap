use super::prelude::*;

fn assert_exits(child: &mut dyn ChildWrapper) -> Result<()> {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if let Some(status) = child.try_wait()? {
			assert!(status.success());
			return Ok(());
		}
		if std::time::Instant::now() >= deadline {
			child.kill()?;
			return Err(std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				"command did not exit before try_wait deadline",
			));
		}
		sleep(Duration::from_millis(10));
	}
}

fn command() -> CommandWrap {
	CommandWrap::with_new("cmd.exe", |command| {
		command
			.args(["/D", "/S", "/C", "exit /b 0"])
			.stdout(Stdio::null());
	})
}

#[test]
fn nowrap() -> Result<()> {
	let mut child = command().spawn()?;
	assert_exits(child.as_mut())
}

#[cfg(feature = "job-object")]
#[test]
fn job_object() -> Result<()> {
	let mut child = command().wrap(JobObject).spawn()?;
	assert_exits(child.as_mut())
}
