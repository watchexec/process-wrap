use std::{
	io::{BufRead, BufReader, ErrorKind},
	process::{Command, Stdio},
	time::Duration,
};

use super::prelude::*;

struct ChildGuard(Box<dyn ChildWrapper>);

impl Drop for ChildGuard {
	fn drop(&mut self) {
		let _ = self.0.start_kill();
		let _ = self.0.wait();
	}
}

fn wrapped_helper(mode: &str) -> Result<ChildGuard> {
	let mut command = CommandWrap::with_new(
		env!("CARGO_BIN_EXE_job-object-accounting-helper"),
		|command| {
			command.arg(mode).stdout(Stdio::piped());
		},
	);
	command.wrap(JobObject);
	Ok(ChildGuard(command.spawn()?))
}

fn read_ready(child: &mut dyn ChildWrapper) -> Result<()> {
	let stdout = child
		.stdout()
		.take()
		.ok_or_else(|| std::io::Error::other("helper stdout was not piped"))?;
	let mut line = String::new();
	BufReader::new(stdout).read_line(&mut line)?;
	if line.trim() == "child-exited" || line.trim() == "ready" {
		Ok(())
	} else {
		Err(std::io::Error::other(
			"helper returned an unexpected readiness line",
		))
	}
}

#[test]
fn job_cpu_time_includes_descendant_that_exited_between_samples() -> Result<()> {
	let mut child = wrapped_helper("cpu-root")?;
	read_ready(child.0.as_mut())?;
	let cpu_time = child.0.job_cpu_time()?;
	assert!(
		cpu_time >= Duration::from_millis(250),
		"terminated descendant CPU was missing from the Job Object total: {cpu_time:?}"
	);
	Ok(())
}

#[test]
fn job_resident_bytes_fail_closed_without_a_coherent_aggregate() -> Result<()> {
	let mut child = wrapped_helper("cpu-root")?;
	read_ready(child.0.as_mut())?;
	let error = child.0.job_resident_bytes().unwrap_err();
	assert_eq!(error.kind(), ErrorKind::Unsupported);
	Ok(())
}

#[test]
fn child_without_job_object_reports_accounting_unavailable() -> Result<()> {
	let mut child = Command::new("cmd.exe")
		.args(["/D", "/S", "/C", "ping -n 6 127.0.0.1 >NUL"])
		.spawn()?;
	let cpu_error = <std::process::Child as ChildWrapper>::job_cpu_time(&child).unwrap_err();
	let memory_error =
		<std::process::Child as ChildWrapper>::job_resident_bytes(&child).unwrap_err();
	child.kill()?;
	let _ = child.wait()?;
	assert_eq!(cpu_error.kind(), ErrorKind::Unsupported);
	assert_eq!(memory_error.kind(), ErrorKind::Unsupported);
	Ok(())
}
