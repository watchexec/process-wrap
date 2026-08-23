#![cfg(all(
	feature = "pty",
	any(
		target_os = "android",
		target_os = "dragonfly",
		target_os = "freebsd",
		target_os = "illumos",
		target_os = "linux",
		target_os = "macos",
		target_os = "netbsd",
		target_os = "openbsd",
		target_os = "solaris"
	)
))]

#[cfg(feature = "process-group")]
use std::any::TypeId;
#[cfg(feature = "reset-sigmask")]
use std::os::unix::process::ExitStatusExt;
use std::{io, process::ExitStatus, time::Duration};

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
use nix::{
	sys::signal::{Signal, kill},
	unistd::Pid,
};

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
use process_wrap::tokio::KillOnDrop;
#[cfg(feature = "process-session")]
use process_wrap::tokio::ProcessSession;
#[cfg(feature = "reset-sigmask")]
use process_wrap::tokio::ResetSigmask;
use process_wrap::tokio::{ChildWrapper, PtyCommand, PtyOptions, PtyOutput, PtySize};
#[cfg(feature = "process-group")]
use process_wrap::tokio::{ProcessGroup, ProcessGroupChild};
#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	time::{sleep, timeout},
};

async fn wait_and_drain(
	child: &mut dyn ChildWrapper,
	output: &mut PtyOutput,
) -> io::Result<(ExitStatus, Vec<u8>)> {
	let mut bytes = Vec::new();
	let status = timeout(Duration::from_secs(5), async {
		let (status, _) = tokio::try_join!(child.wait(), output.read_to_end(&mut bytes))?;
		Ok::<_, io::Error>(status)
	})
	.await??;
	Ok((status, bytes))
}

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
	let (status, bytes) = wait_and_drain(child.as_mut(), &mut output).await?;
	assert!(status.success());
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
	let expected_directory = directory.path().canonicalize()?;
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
	let (status, bytes) = wait_and_drain(child.as_mut(), &mut output).await?;
	assert!(status.success());
	assert_eq!(
		String::from_utf8(bytes).unwrap(),
		format!(
			"argument|environment|unset|unset|{}",
			expected_directory.display()
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

#[cfg(not(target_os = "macos"))]
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
			"trap '' HUP; (printf ready; while [ ! -e \"$1\" ]; do sleep 1; done) &",
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
	let (status, bytes) = wait_and_drain(child.as_mut(), &mut output).await?;
	assert!(status.success());
	assert_eq!(bytes, b"reused");
	Ok(())
}

#[tokio::test]
async fn kill_and_start_kill_preserve_repeated_waits() -> io::Result<()> {
	for wait_in_kill in [false, true] {
		let mut command = PtyCommand::new("sh");
		command.args(["-c", "stty -echo; printf ready; while :; do sleep 1; done"]);
		let (mut child, controller) = command.spawn(PtyOptions::default())?;
		let (input, mut output, _resize) = controller.into_parts();
		let mut ready = [0; 5];
		timeout(Duration::from_secs(5), output.read_exact(&mut ready)).await??;
		assert_eq!(&ready, b"ready");
		drop(input);

		if wait_in_kill {
			timeout(Duration::from_secs(5), Box::into_pin(child.kill())).await??;
		} else {
			child.start_kill()?;
			timeout(Duration::from_secs(5), child.wait()).await??;
		}
		let status = child.try_wait()?.expect("killed child must have exited");
		assert_eq!(child.try_wait()?, Some(status));
		assert_eq!(child.wait().await?, status);
		let mut bytes = Vec::new();
		timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	}
	Ok(())
}

#[cfg(feature = "process-group")]
async fn assert_group_signal(mut command: PtyCommand) -> io::Result<()> {
	command.args([
		"-c",
		"stty -echo; trap '' HUP; trap 'exit 0' TERM; sleep 30 & printf ready; wait",
	]);
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	assert_eq!(child.as_ref().type_id(), TypeId::of::<ProcessGroupChild>());
	assert!(child.try_wait()?.is_none());

	let (input, mut output, _resize) = controller.into_parts();
	let mut ready = [0; 5];
	timeout(Duration::from_secs(5), output.read_exact(&mut ready)).await??;
	assert_eq!(&ready, b"ready");
	drop(input);

	child.signal(nix::libc::SIGTERM)?;
	let status = timeout(Duration::from_secs(5), child.wait()).await??;
	assert_eq!(child.try_wait()?, Some(status));
	assert_eq!(child.try_wait()?, Some(status));
	assert_eq!(child.wait().await?, status);
	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	Ok(())
}

#[cfg(feature = "process-group")]
#[tokio::test]
async fn process_group_leader_preserves_pty_group_supervision() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.wrap(ProcessGroup::leader());
	assert_group_signal(command).await
}

#[cfg(feature = "process-group")]
#[tokio::test]
async fn process_group_try_wait_keeps_native_child_synchronized() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.args(["-c", "exit 17"]).wrap(ProcessGroup::leader());
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);

	let status = timeout(Duration::from_secs(5), async {
		loop {
			if let Some(status) = child.try_wait()? {
				break Ok::<_, io::Error>(status);
			}
			sleep(Duration::from_millis(10)).await;
		}
	})
	.await??;
	let native =
		unsafe { child.try_inner_child_mut() }.expect("PTY group child must contain Tokio Child");
	assert_eq!(native.try_wait()?, Some(status));
	assert_eq!(child.try_wait()?, Some(status));
	assert_eq!(child.wait().await?, status);
	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	Ok(())
}

#[cfg(feature = "process-group")]
#[tokio::test]
async fn process_group_composes_when_registered_after_first_spawn() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.args(["-c", "printf reused"]);

	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let (status, bytes) = wait_and_drain(child.as_mut(), &mut output).await?;
	assert!(status.success());
	assert_eq!(bytes, b"reused");

	command.wrap(ProcessGroup::leader());
	let (mut child, controller) = command.spawn(PtyOptions::default())?;
	assert_eq!(child.as_ref().type_id(), TypeId::of::<ProcessGroupChild>());
	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let (status, bytes) = wait_and_drain(child.as_mut(), &mut output).await?;
	assert!(status.success());
	assert_eq!(bytes, b"reused");
	Ok(())
}

#[cfg(feature = "process-session")]
#[tokio::test]
async fn process_session_preserves_pty_group_supervision() -> io::Result<()> {
	let mut command = PtyCommand::new("sh");
	command.wrap(ProcessSession);
	assert_group_signal(command).await
}

#[cfg(feature = "process-group")]
#[tokio::test]
async fn rejects_attaching_a_pty_to_an_existing_group() {
	for group in [ProcessGroup::attach_to(0), ProcessGroup::attach_to(42)] {
		let mut command = PtyCommand::new("sh");
		command.args(["-c", "exit 0"]).wrap(group);
		assert_eq!(
			command.spawn(PtyOptions::default()).unwrap_err().kind(),
			io::ErrorKind::InvalidInput
		);
	}
}

#[cfg(feature = "process-session")]
#[tokio::test]
async fn rejects_explicit_group_and_session_in_either_order() {
	for session_first in [false, true] {
		let mut command = PtyCommand::new("sh");
		command.args(["-c", "exit 0"]);
		if session_first {
			command.wrap(ProcessSession).wrap(ProcessGroup::leader());
		} else {
			command.wrap(ProcessGroup::leader()).wrap(ProcessSession);
		}
		assert_eq!(
			command.spawn(PtyOptions::default()).unwrap_err().kind(),
			io::ErrorKind::InvalidInput
		);
	}
}

#[cfg(feature = "reset-sigmask")]
#[tokio::test]
async fn reset_sigmask_unblocks_signals_before_pty_setup() -> io::Result<()> {
	use nix::sys::signal::{SigSet, SigmaskHow, Signal, sigprocmask};

	let mut blocked = SigSet::empty();
	blocked.add(Signal::SIGUSR1);
	let mut previous = SigSet::empty();
	sigprocmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))?;

	let mut command = PtyCommand::new("sh");
	command
		.args(["-c", "test -t 0 || exit 2; kill -USR1 $$; printf survived"])
		.wrap(ResetSigmask);
	let spawned = command.spawn(PtyOptions::default());
	sigprocmask(SigmaskHow::SIG_SETMASK, Some(&previous), None)?;
	let (mut child, controller) = spawned?;

	let (input, mut output, _resize) = controller.into_parts();
	drop(input);
	let status = timeout(Duration::from_secs(5), child.wait()).await??;
	assert_eq!(status.signal(), Some(Signal::SIGUSR1 as i32));
	let mut bytes = Vec::new();
	timeout(Duration::from_secs(5), output.read_to_end(&mut bytes)).await??;
	assert!(bytes.is_empty());
	Ok(())
}

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
fn pid_alive(pid: Pid) -> bool {
	!matches!(kill(pid, None), Err(nix::errno::Errno::ESRCH))
}

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
struct KillPid(Option<Pid>);

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
impl KillPid {
	fn new(pid: Pid) -> Self {
		Self(Some(pid))
	}

	fn disarm(&mut self) {
		self.0 = None;
	}
}

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
impl Drop for KillPid {
	fn drop(&mut self) {
		if let Some(pid) = self.0 {
			let _ = kill(pid, Signal::SIGKILL);
		}
	}
}

#[cfg(all(feature = "kill-on-drop", feature = "process-session"))]
#[tokio::test]
async fn kill_on_drop_remains_direct_child_only_with_a_pty_session() -> io::Result<()> {
	let directory = tempfile::tempdir()?;
	let ready = directory.path().join("descendant-ready");
	let acknowledged = directory.path().join("descendant-acknowledged");
	let mut command = PtyCommand::new("sh");
	command
		.args([
			"-c",
			r#"stty -echo; trap '' HUP; (trap '' HUP; trap ': > "$2"; exit 0' USR1; : > "$1"; while :; do sleep 1; done) & printf '%s:%s\n' "$$" "$!"; wait"#,
			"pty-test",
		])
		.arg(&ready)
		.arg(&acknowledged)
		.wrap(ProcessSession)
		.wrap(KillOnDrop);
	let (child, controller) = command.spawn(PtyOptions::default())?;
	let (input, output, _resize) = controller.into_parts();
	let mut output = BufReader::new(output);
	let mut line = String::new();
	timeout(Duration::from_secs(5), output.read_line(&mut line)).await??;
	let (direct, descendant) = line.trim().split_once(':').unwrap();
	let direct = Pid::from_raw(direct.parse().unwrap());
	let descendant = Pid::from_raw(descendant.parse().unwrap());
	assert_eq!(child.id(), Some(direct.as_raw() as u32));
	let mut direct_cleanup = KillPid::new(direct);
	let mut descendant_cleanup = KillPid::new(descendant);
	assert!(pid_alive(direct));
	timeout(Duration::from_secs(5), async {
		while !ready.exists() {
			sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("descendant did not install its signal handler");

	drop(input);
	drop(child);
	timeout(Duration::from_secs(5), async {
		while pid_alive(direct) {
			sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("kill-on-drop did not terminate the direct child");
	direct_cleanup.disarm();

	kill(descendant, Signal::SIGUSR1)?;
	timeout(Duration::from_secs(5), async {
		while !acknowledged.exists() {
			sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("descendant did not acknowledge its signal");
	timeout(Duration::from_secs(5), async {
		while pid_alive(descendant) {
			sleep(Duration::from_millis(10)).await;
		}
	})
	.await
	.expect("acknowledged descendant did not exit");
	descendant_cleanup.disarm();
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
