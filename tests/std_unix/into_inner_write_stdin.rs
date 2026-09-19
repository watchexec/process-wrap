use super::prelude::*;

#[test]
fn nowrap() -> Result<()> {
	// SAFETY: this `CommandWrap` has no registered wrapper, so spawn returns its direct native
	// `cat` child. There is no cleanup or supervision layer to remove before taking it.
	let mut child = unsafe {
		CommandWrap::with_new("cat", |command| {
			command.stdin(Stdio::piped()).stdout(Stdio::piped());
		})
		.spawn()?
		.try_into_inner_child()
	}
	.map_err(|_| std::io::Error::other("spawned wrapper chain did not end in a native child"))?;

	if let Some(mut din) = child.stdin.take() {
		din.write_all(b"hello")?;
	}

	let mut output = String::new();
	if let Some(mut out) = child.stdout.take() {
		out.read_to_string(&mut output)?;
	}

	assert_eq!(output.as_str(), "hello");
	Ok(())
}

#[cfg(feature = "process-group")]
#[test]
fn process_group() -> Result<()> {
	// SAFETY: this fixture's sole layer is `ProcessGroupChild -> native cat child`. Its `cat`
	// command has no descendants; closing stdin and draining stdout make that child exit, and the
	// test performs no later group-supervision action after consuming the wrapper.
	let mut child = unsafe {
		CommandWrap::with_new("cat", |command| {
			command.stdin(Stdio::piped()).stdout(Stdio::piped());
		})
		.wrap(ProcessGroup::leader())
		.spawn()?
		.try_into_inner_child()
	}
	.map_err(|_| std::io::Error::other("spawned wrapper chain did not end in a native child"))?;

	if let Some(mut din) = child.stdin.take() {
		din.write_all(b"hello")?;
	}

	let mut output = String::new();
	if let Some(mut out) = child.stdout.take() {
		out.read_to_string(&mut output)?;
	}

	assert_eq!(output.as_str(), "hello");
	Ok(())
}

#[cfg(feature = "process-session")]
#[test]
fn process_session() -> Result<()> {
	// SAFETY: this fixture's sole layer is `ProcessGroupChild -> native cat child`, created by
	// `ProcessSession`. `cat` has no descendants; closing stdin and draining stdout make it exit,
	// and the test performs no later group-supervision action after consuming the wrapper.
	let mut child = unsafe {
		CommandWrap::with_new("cat", |command| {
			command.stdin(Stdio::piped()).stdout(Stdio::piped());
		})
		.wrap(ProcessSession)
		.spawn()?
		.try_into_inner_child()
	}
	.map_err(|_| std::io::Error::other("spawned wrapper chain did not end in a native child"))?;

	if let Some(mut din) = child.stdin.take() {
		din.write_all(b"hello")?;
	}

	let mut output = String::new();
	if let Some(mut out) = child.stdout.take() {
		out.read_to_string(&mut output)?;
	}

	assert_eq!(output.as_str(), "hello");
	Ok(())
}
