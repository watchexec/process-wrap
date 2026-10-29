use std::io::ErrorKind;

use remoteprocess::Pid;

#[must_use]
#[track_caller]
pub fn pid_alive(pid: Pid) -> bool {
	match remoteprocess::Process::new(pid).and_then(|p| p.threads()) {
		Ok(threads) => !threads.is_empty(),
		Err(remoteprocess::Error::IOError(e)) => {
			if e.kind() == ErrorKind::NotFound ||
			// macos returns os error 0 on unrecognized pids
			e.raw_os_error() == Some(0)
			{
				return false;
			}
			panic!("remoteprocess encountered an io error: {e}")
		}
		Err(other) => panic!("remoteprocess failed to get info: {other}"),
	}
}
