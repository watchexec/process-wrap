#![cfg(all(any(target_os = "linux", target_os = "macos"), feature = "pty"))]

use std::{io, time::Duration};

use process_wrap::tokio::{PtyCommand, PtyOptions, PtySize};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	time::{sleep, timeout},
};

#[tokio::test]
async fn spawns_with_terminal_fds_and_merged_output() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.args([
		"-c",
		"test -t 0 && printf stdin-tty; test -t 1 && printf stdout-tty; test -t 2 && printf stderr-tty >&2; printf stdout; printf stderr >&2",
	]);

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let status = timeout(Duration::from_secs(5), child.wait()).await??;
	assert!(status.success());

	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	assert_eq!(bytes, b"stdin-ttystdout-ttystderr-ttystdoutstderr");
	Ok(())
}

#[tokio::test]
async fn passes_bidirectional_control_bytes_unchanged() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.args([
		"-c",
		"stty raw -echo; printf ready; dd bs=1 count=4 2>/dev/null",
	]);

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (mut input, mut output, _resize) = controller.into_parts();
	let mut ready = [0; 5];
	timeout(Duration::from_secs(5), output.read_exact(&mut ready)).await??;
	assert_eq!(&ready, b"ready");

	let controls = [0x00, 0x1b, 0x03, 0xff];
	input.write_all(&controls).await?;
	let mut returned = [0; 4];
	timeout(Duration::from_secs(5), output.read_exact(&mut returned)).await??;
	assert_eq!(returned, controls);
	input.shutdown().await?;
	assert!(
		timeout(Duration::from_secs(5), child.wait())
			.await??
			.success()
	);
	Ok(())
}

#[tokio::test]
async fn preserves_tracked_command_intent() -> io::Result<()> {
	let directory = tempfile::tempdir()?;
	let mut command = PtyCommand::new("/bin/sh");
	command
		.args([
			"-c",
			"printf '%s|%s|%s|%s|%s' \"$1\" \"$VALUE\" \"${BEFORE-unset}\" \"${REMOVE-unset}\" \"$PWD\"",
			"pty-test",
			"argument",
		])
		.env("BEFORE", "discarded")
		.env_clear()
		.env("VALUE", "environment")
		.env("REMOVE", "discarded")
		.env_remove("REMOVE")
		.current_dir(directory.path());

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	assert!(
		timeout(Duration::from_secs(5), child.wait())
			.await??
			.success()
	);

	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	assert_eq!(
		String::from_utf8(bytes).unwrap(),
		format!(
			"argument|environment|unset|unset|{}",
			directory.path().display()
		)
	);
	Ok(())
}

#[tokio::test]
async fn reports_initial_size_and_sigwinch_resize() -> io::Result<()> {
	let initial = PtySize::new(31, 97)?.with_pixels(640, 480);
	let mut command = PtyCommand::new("sh");
	command.args([
		"-c",
		"stty -echo; stty size; trap 'stty size; exit 0' WINCH; printf ready; while :; do sleep 1; done",
	]);

	let (mut child, controller) = command.spawn(PtyOptions::new(initial))?;
	let (input, mut output, resize) = controller.into_parts();
	let mut initial_output = [0; 12];
	timeout(
		Duration::from_secs(5),
		output.read_exact(&mut initial_output),
	)
	.await??;
	assert_eq!(&initial_output, b"31 97\r\nready");

	assert_eq!(
		resize
			.resize(PtySize {
				rows: 0,
				columns: 113,
				pixel_width: 0,
				pixel_height: 0,
			})
			.unwrap_err()
			.kind(),
		io::ErrorKind::InvalidInput
	);
	resize.resize(PtySize::new(42, 113)?)?;
	let mut resized = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut resized)).await??;
	assert_eq!(resized, b"42 113\r\n");
	assert!(
		timeout(Duration::from_secs(5), child.wait())
			.await??
			.success()
	);
	drop(input);
	Ok(())
}

#[tokio::test]
async fn shutting_down_input_does_not_hang_up_while_output_exists() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.args(["-c", "stty -echo; printf ready; IFS= read -r line"]);

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (mut input, mut output, resize) = controller.into_parts();
	let mut ready = [0; 5];
	timeout(Duration::from_secs(5), output.read_exact(&mut ready)).await??;
	assert_eq!(&ready, b"ready");

	input.shutdown().await?;
	assert_eq!(
		input.write_all(b"closed").await.unwrap_err().kind(),
		io::ErrorKind::BrokenPipe
	);
	sleep(Duration::from_millis(50)).await;
	assert!(child.try_wait()?.is_none());
	resize.resize(PtySize::default())?;

	drop(output);
	assert_eq!(
		resize.resize(PtySize::default()).unwrap_err().kind(),
		io::ErrorKind::BrokenPipe
	);
	timeout(Duration::from_secs(5), child.wait()).await??;
	Ok(())
}

#[tokio::test]
async fn dropping_output_does_not_hang_up_while_input_exists() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.args(["-c", "stty -echo; printf ready; IFS= read -r line"]);

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, resize) = controller.into_parts();
	let mut ready = [0; 5];
	timeout(Duration::from_secs(5), output.read_exact(&mut ready)).await??;
	drop(output);

	sleep(Duration::from_millis(50)).await;
	assert!(child.try_wait()?.is_none());
	resize.resize(PtySize::default())?;
	drop(input);
	assert_eq!(
		resize.resize(PtySize::default()).unwrap_err().kind(),
		io::ErrorKind::BrokenPipe
	);
	timeout(Duration::from_secs(5), child.wait()).await??;
	Ok(())
}

#[tokio::test]
async fn direct_child_wait_is_independent_from_descendant_output_eof() -> io::Result<()> {
	struct ReleaseOnDrop(std::path::PathBuf);

	impl Drop for ReleaseOnDrop {
		fn drop(&mut self) {
			let _ = std::fs::File::create(&self.0);
		}
	}

	let directory = tempfile::tempdir()?;
	let release = directory.path().join("release-descendant");
	let _release_on_drop = ReleaseOnDrop(release.clone());
	let mut command = PtyCommand::new("sh");
	command
		.args([
			"-c",
			"(trap '' HUP; printf ready; while [ ! -e \"$1\" ]; do sleep 1; done) &",
			"pty-test",
		])
		.arg(&release);

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let mut ready = [0; 5];
	timeout(Duration::from_secs(5), output.read_exact(&mut ready)).await??;
	assert_eq!(&ready, b"ready");
	assert!(
		timeout(Duration::from_secs(5), child.wait())
			.await??
			.success()
	);

	let mut bytes = Vec::new();
	assert!(
		timeout(Duration::from_millis(100), output.read_to_end(&mut bytes))
			.await
			.is_err()
	);
	std::fs::File::create(&release)?;
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	Ok(())
}

#[tokio::test]
async fn failed_spawn_leaves_command_reusable() -> io::Result<()> {
	let mut command = PtyCommand::new("/process-wrap/definitely-does-not-exist");
	assert_eq!(
		command.spawn(PtyOptions::default()).unwrap_err().kind(),
		io::ErrorKind::NotFound
	);

	command.program("sh").args(["-c", "printf reused"]);
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	assert!(
		timeout(Duration::from_secs(5), child.wait())
			.await??
			.success()
	);
	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	assert_eq!(bytes, b"reused");
	Ok(())
}

#[test]
fn validates_character_dimensions() {
	assert_eq!(
		PtySize::new(0, 80).unwrap_err().kind(),
		io::ErrorKind::InvalidInput
	);
	assert_eq!(
		PtySize::new(24, 0).unwrap_err().kind(),
		io::ErrorKind::InvalidInput
	);
}

#[test]
fn validates_spawn_options_before_opening_a_pty() {
	let mut command = PtyCommand::new("ignored");
	let options = PtyOptions::new(PtySize {
		rows: 0,
		columns: 80,
		pixel_width: 0,
		pixel_height: 0,
	});
	assert_eq!(
		command.spawn(options).unwrap_err().kind(),
		io::ErrorKind::InvalidInput
	);
}
