use std::{
	black_box,
	io::{self, Write},
	process::{Command, Stdio},
	thread,
	time::{Duration, Instant},
};

fn cpu_root() -> io::Result<()> {
	let mut worker = Command::new(std::env::current_exe()?)
		.arg("cpu-worker")
		.stdout(Stdio::null())
		.spawn()?;
	worker.wait()?;
	writeln!(io::stdout().lock(), "child-exited")?;
	loop {
		thread::sleep(Duration::from_secs(60));
	}
}

fn cpu_worker() {
	let deadline = Instant::now() + Duration::from_millis(700);
	let mut value = 0_u64;
	while Instant::now() < deadline {
		value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
		black_box(value);
	}
}

fn run() -> io::Result<()> {
	match std::env::args().nth(1).as_deref() {
		Some("cpu-root") => cpu_root()?,
		Some("cpu-worker") => cpu_worker(),
		_ => return Err(io::Error::other("expected helper mode").into()),
	}
	Ok(())
}

fn main() {
	if let Err(error) = run() {
		eprintln!("{error}");
		std::process::exit(1);
	}
}
