use std::sync::{
	atomic::{AtomicU32, Ordering},
	Arc,
};

use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use super::prelude::*;

#[derive(Clone, Debug, Default)]
pub struct FlagSpy {
	pub flags: Arc<AtomicU32>,
}

impl StdCommandWrapper for FlagSpy {
	fn pre_spawn(&mut self, _command: &mut Command, core: &StdCommandWrap) -> Result<()> {
		#[cfg(feature = "creation-flags")]
		if let Some(CreationFlags(user_flags)) = core.get_wrap::<CreationFlags>().as_deref() {
			self.flags.store(user_flags.0, Ordering::Relaxed);
		}

		Ok(())
	}
}

#[test]
fn retrieve_flags() -> Result<()> {
	super::init();

	let spy = FlagSpy::default();
	let _ = StdCommandWrap::with_new("powershell.exe", |command| {
		command.arg("/C").arg("echo hello").stdout(Stdio::piped());
	})
	.wrap(CreationFlags(CREATE_NO_WINDOW))
	.wrap(spy.clone())
	.spawn()?;

	assert_eq!(spy.flags.load(Ordering::Relaxed), CREATE_NO_WINDOW.0);

	Ok(())
}
