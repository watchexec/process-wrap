#[cfg(windows)]
fn example() -> Result<(), Box<dyn std::error::Error>> {
	use process_wrap::std::{Command, CreationFlags, JobObject};
	use windows::Win32::System::Threading::{CREATE_NO_WINDOW, DETACHED_PROCESS};

	Command::with_new("watch", |command| {
		command.arg("ls");
	})
	.wrap(CreationFlags(CREATE_NO_WINDOW | DETACHED_PROCESS))
	.wrap(JobObject)
	.spawn()?;
	Ok(())
}

fn main() {
	#[cfg(windows)]
	let _ = example();
}
