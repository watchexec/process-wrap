#![cfg(feature = "process-group")]

use std::any::Any;

use nix::unistd::{Pid, getpgid};

use super::prelude::*;

fn sleeper() -> CommandWrap {
	CommandWrap::with_new("sleep", |command| {
		command.arg("10");
	})
}

#[test]
fn attach_to_a_leaders_group() -> Result<()> {
	let mut leader = sleeper().wrap(ProcessGroup::leader()).spawn()?;
	let pgid = (leader.as_ref() as &dyn Any)
		.downcast_ref::<ProcessGroupChild>()
		.expect("ProcessGroup is the outermost wrapper")
		.pgid();
	assert_eq!(pgid, leader.id(), "the leader's group is its own");

	let member = sleeper().wrap(ProcessGroup::attach_to(pgid)).spawn()?;
	let member_pgid = getpgid(Some(Pid::from_raw(member.id() as _)))?;
	assert_eq!(member_pgid.as_raw() as u32, pgid);

	// Killing the group takes the member with it; the leader's wait reaps both.
	leader.start_kill()?;
	let status = leader.wait()?;
	assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
	drop(member);
	Ok(())
}
