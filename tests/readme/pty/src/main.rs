use process_wrap::tokio::{Command, Pty};
use tokio::io::AsyncReadExt;

async fn example() -> Result<(), Box<dyn std::error::Error>> {
	#[cfg(unix)]
	let mut command = Command::with_new("sh", |command| {
		command.args(["-c", "printf terminal"]);
	});
	#[cfg(windows)]
	let mut command = Command::with_new("cmd.exe", |command| {
		command.args(["/d", "/s", "/c", "echo terminal"]);
	});
	command.wrap(Pty::default());
	let mut child = command.spawn()?;
	let controller = child
		.take_pty_controller()
		.expect("a successful PTY spawn installs one controller");
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);

	let drain = tokio::spawn(async move {
		let mut bytes = Vec::new();
		output.read_to_end(&mut bytes).await?;
		Ok::<_, std::io::Error>(bytes)
	});
	let status = child.wait().await?;
	let terminal_bytes = drain.await??;
	dbg!(status, terminal_bytes);
	Ok(())
}

fn main() {
	let _ = example;
}
