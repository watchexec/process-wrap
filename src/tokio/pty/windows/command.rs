use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt};

use super::super::{ArgIntent, CommandIntent};

const BACKSLASH: u16 = b'\\' as u16;
const DOUBLE_QUOTE: u16 = b'"' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WideCString(Vec<u16>);

impl WideCString {
	fn from_units(mut units: Vec<u16>) -> Self {
		debug_assert!(!units.contains(&0));
		units.push(0);
		Self(units)
	}

	pub(super) fn as_ptr(&self) -> *const u16 {
		self.0.as_ptr()
	}

	pub(super) fn as_mut_ptr(&mut self) -> *mut u16 {
		self.0.as_mut_ptr()
	}

	pub(super) fn as_units(&self) -> &[u16] {
		&self.0
	}
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct PreparedCommandLine {
	pub(super) application_name: WideCString,
	pub(super) command_line: WideCString,
}

pub(super) fn prepare_command_line(intent: &CommandIntent) -> io::Result<PreparedCommandLine> {
	let program = encode(&intent.program, "PTY program")?;
	let application_name = WideCString::from_units(program.clone());
	let mut command_line = Vec::new();
	append_regular(&mut command_line, &program, true);

	for arg in &intent.args {
		command_line.push(SPACE);
		match arg {
			ArgIntent::Regular(arg) => {
				let arg = encode(arg, "PTY argument")?;
				append_regular(&mut command_line, &arg, false);
			}
			ArgIntent::Raw(arg) => {
				command_line.extend(encode(arg, "PTY raw argument")?);
			}
		}
	}

	Ok(PreparedCommandLine {
		application_name,
		command_line: WideCString::from_units(command_line),
	})
}

fn encode(value: &OsStr, field: &'static str) -> io::Result<Vec<u16>> {
	let units = value.encode_wide().collect::<Vec<_>>();
	if units.contains(&0) {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("{field} contains an embedded NUL"),
		));
	}
	Ok(units)
}

fn append_regular(command_line: &mut Vec<u16>, arg: &[u16], force_quotes: bool) {
	let quoted =
		force_quotes || arg.is_empty() || arg.iter().any(|unit| matches!(*unit, SPACE | TAB));
	if quoted {
		command_line.push(DOUBLE_QUOTE);
	}

	let mut backslashes = 0;
	for &unit in arg {
		if unit == BACKSLASH {
			backslashes += 1;
			continue;
		}

		append_backslashes(command_line, backslashes);
		if unit == DOUBLE_QUOTE {
			append_backslashes(command_line, backslashes);
			command_line.push(BACKSLASH);
		}
		command_line.push(unit);
		backslashes = 0;
	}

	append_backslashes(command_line, backslashes);
	if quoted {
		append_backslashes(command_line, backslashes);
		command_line.push(DOUBLE_QUOTE);
	}
}

fn append_backslashes(command_line: &mut Vec<u16>, count: usize) {
	for _ in 0..count {
		command_line.push(BACKSLASH);
	}
}
