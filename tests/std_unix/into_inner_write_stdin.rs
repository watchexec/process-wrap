use super::prelude::*;

#[test]
fn nowrap() -> Result<()> {
	// SAFETY: there is no wrapper layer to bypass.
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
	// SAFETY: the fixture has no descendants or later group-supervision step to bypass.
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
	// SAFETY: the fixture has no descendants or later group-supervision step to bypass.
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
