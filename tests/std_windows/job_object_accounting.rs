use std::{
	hint::black_box,
	io::{BufRead, BufReader, ErrorKind, Write},
	process::{Command, Stdio},
	thread,
	time::{Duration, Instant},
};

use super::prelude::*;

struct ChildGuard(Box<dyn ChildWrapper>);

impl Drop for ChildGuard {
	fn drop(&mut self) {
		let _ = self.0.start_kill();
		let _ = self.0.wait();
	}
}

fn helper_test_filter() -> String {
	format!("{}::helper_entrypoint", module_path!())
}

fn wrapped_helper(mode: &str) -> Result<ChildGuard> {
	let executable = std::env::current_exe()?;
	let filter = helper_test_filter();
	let mut command = CommandWrap::with_new(executable, |command| {
		command
			.args(["--exact", &filter, "--nocapture"])
			.env("PROCESS_WRAP_ACCOUNTING_HELPER", mode)
			.stdout(Stdio::piped());
	});
	command.wrap(JobObject);
	Ok(ChildGuard(command.spawn()?))
}

fn read_ready(child: &mut dyn ChildWrapper) -> Result<()> {
	let stdout = child
		.stdout()
		.take()
		.ok_or_else(|| std::io::Error::other("helper stdout was not piped"))?;
	let mut reader = BufReader::new(stdout);
	let mut line = String::new();
	loop {
		line.clear();
		if reader.read_line(&mut line)? == 0 {
			return Err(std::io::Error::other(
				"helper exited before reporting readiness",
			));
		}
		if line.trim() == "child-exited" || line.trim() == "ready" {
			return Ok(());
		}
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

#[test]
fn helper_entrypoint() -> Result<()> {
	let Some(mode) = std::env::var_os("PROCESS_WRAP_ACCOUNTING_HELPER") else {
		return Ok(());
	};

	match mode.to_str() {
		Some("cpu-root") => {
			let executable = std::env::current_exe()?;
			let filter = helper_test_filter();
			let mut worker = Command::new(executable)
				.args(["--exact", &filter, "--nocapture"])
				.env("PROCESS_WRAP_ACCOUNTING_HELPER", "cpu-worker")
				.stdout(Stdio::null())
				.spawn()?;
			worker.wait()?;
			writeln!(std::io::stdout().lock(), "child-exited")?;
			loop {
				thread::sleep(Duration::from_secs(60));
			}
		}
		Some("cpu-worker") => {
			let deadline = Instant::now() + Duration::from_millis(700);
			let mut value = 0_u64;
			while Instant::now() < deadline {
				value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
				black_box(value);
			}
			Ok(())
		}
		_ => Err(std::io::Error::other("expected helper mode").into()),
	}
}
