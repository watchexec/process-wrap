use std::{
	io::{Error, ErrorKind},
	process::ExitStatus,
	time::Instant,
};

use windows::Win32::System::Threading::{
	CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_CREATION_FLAGS,
};

use super::{prelude::*, windows_thread::process_has_suspended_thread};

const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Order {
	CreationFlagsFirst,
	JobObjectFirst,
}

fn command(flags: PROCESS_CREATION_FLAGS, order: Order) -> CommandWrap {
	let mut command = CommandWrap::with_new("cmd.exe", |command| {
		command.args(["/D", "/S", "/C", "exit /b 0"]);
	});
	match order {
		Order::CreationFlagsFirst => {
			command.wrap(CreationFlags(flags)).wrap(JobObject);
		}
		Order::JobObjectFirst => {
			command.wrap(JobObject).wrap(CreationFlags(flags));
		}
	}
	command
}

fn wait_for_exit(child: &mut dyn ChildWrapper) -> Result<ExitStatus> {
	let deadline = Instant::now() + EXIT_TIMEOUT;
	loop {
		if let Some(status) = child.try_wait()? {
			return Ok(status);
		}
		if Instant::now() >= deadline {
			let _ = child.start_kill();
			return Err(Error::new(ErrorKind::TimedOut, "child did not exit"));
		}
		sleep(Duration::from_millis(10));
	}
}

#[test]
fn preserves_flags_and_resumes_in_both_orders() -> Result<()> {
	let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
	for order in [Order::CreationFlagsFirst, Order::JobObjectFirst] {
		let mut command = command(flags, order);
		assert_eq!(command.get_wrap::<CreationFlags>().unwrap().0, flags);
		let mut child = command.spawn_with(|command| {
			let mut child = command.spawn()?;
			let suspended = match process_has_suspended_thread(child.id()) {
				Ok(suspended) => suspended,
				Err(error) => {
					let _ = child.kill();
					return Err(error);
				}
			};
			if suspended {
				Ok(child)
			} else {
				let _ = child.kill();
				Err(Error::other("child was not created suspended"))
			}
		})?;
		assert_eq!(command.get_wrap::<CreationFlags>().unwrap().0, flags);
		assert!(wait_for_exit(&mut *child)?.success());
	}
	Ok(())
}

#[test]
fn leaves_explicit_suspension_in_both_orders() -> Result<()> {
	let flags = CREATE_NO_WINDOW | CREATE_SUSPENDED;
	for order in [Order::CreationFlagsFirst, Order::JobObjectFirst] {
		let mut command = command(flags, order);
		let mut child = command.spawn()?;
		let remained_suspended = match process_has_suspended_thread(child.id()) {
			Ok(suspended) => suspended,
			Err(error) => {
				let _ = child.start_kill();
				return Err(error);
			}
		};
		child.start_kill()?;
		let _ = wait_for_exit(&mut *child)?;
		assert!(remained_suspended);
		assert_eq!(command.get_wrap::<CreationFlags>().unwrap().0, flags);
	}
	Ok(())
}
