//! Win32 application-name and mutable command-line encoding.
//!
//! Program and argument data stays in WTF-16. Regular arguments follow the Microsoft C runtime's
//! backslash-and-quote rules, while raw fragments are appended unchanged and in registration order.

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

	pub(super) fn from_os(value: &OsStr, field: &'static str) -> io::Result<Self> {
		Ok(Self::from_units(encode(value, field)?))
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

pub(super) fn encode(value: &OsStr, field: &'static str) -> io::Result<Vec<u16>> {
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

#[cfg(test)]
mod tests {
	use std::{
		ffi::OsString,
		os::windows::ffi::{OsStrExt, OsStringExt},
	};

	use super::super::super::EnvironmentIntent;
	use super::*;

	fn os(units: &[u16]) -> OsString {
		OsString::from_wide(units)
	}

	fn regular(value: &str) -> ArgIntent {
		ArgIntent::Regular(OsString::from(value))
	}

	fn raw(value: &str) -> ArgIntent {
		ArgIntent::Raw(OsString::from(value))
	}

	fn intent(program: OsString, args: Vec<ArgIntent>) -> CommandIntent {
		CommandIntent {
			program,
			args,
			environment: EnvironmentIntent {
				clear: false,
				changes: Vec::new(),
			},
			current_dir: None,
		}
	}

	fn terminated(value: &str) -> Vec<u16> {
		OsStr::new(value).encode_wide().chain([0]).collect()
	}

	#[test]
	fn quotes_regular_arguments_with_windows_crt_rules() {
		let cases = [
			("none", Vec::new(), "\"tool\""),
			("empty", vec![regular("")], "\"tool\" \"\""),
			("simple", vec![regular("plain")], "\"tool\" plain"),
			(
				"space",
				vec![regular("two words")],
				"\"tool\" \"two words\"",
			),
			(
				"tab",
				vec![regular("two\twords")],
				"\"tool\" \"two\twords\"",
			),
			("quote", vec![regular("a\"b")], "\"tool\" a\\\"b"),
			(
				"backslash quote",
				vec![regular("a\\\"b")],
				"\"tool\" a\\\\\\\"b",
			),
			("unquoted slash", vec![regular("a\\")], "\"tool\" a\\"),
			(
				"quoted trailing slash",
				vec![regular("a b\\")],
				"\"tool\" \"a b\\\\\"",
			),
		];

		for (name, args, expected) in cases {
			let prepared = prepare_command_line(&intent(OsString::from("tool"), args)).unwrap();
			assert_eq!(
				prepared.command_line.as_units(),
				terminated(expected),
				"{name}"
			);
			assert_eq!(
				prepared.application_name.as_units(),
				terminated("tool"),
				"{name}"
			);
		}
	}

	#[test]
	fn preserves_interleaved_raw_fragments_exactly() {
		let prepared = prepare_command_line(&intent(
			OsString::from("tool"),
			vec![
				regular("two words"),
				raw(r#"/D "literal""#),
				regular("plain"),
				raw(""),
			],
		))
		.unwrap();
		assert_eq!(
			prepared.command_line.as_units(),
			terminated(r#""tool" "two words" /D "literal" plain "#)
		);
	}

	#[test]
	fn preserves_lone_surrogates_in_program_and_arguments() {
		let program = [b't' as u16, 0xd800, b'o' as u16];
		let prepared = prepare_command_line(&intent(
			os(&program),
			vec![
				ArgIntent::Regular(os(&[0xdc00])),
				ArgIntent::Raw(os(&[0xd801])),
			],
		))
		.unwrap();

		assert_eq!(
			prepared.application_name.as_units(),
			[program.as_slice(), &[0]].concat()
		);
		assert_eq!(
			prepared.command_line.as_units(),
			[
				DOUBLE_QUOTE,
				program[0],
				program[1],
				program[2],
				DOUBLE_QUOTE,
				SPACE,
				0xdc00,
				SPACE,
				0xd801,
				0,
			]
		);
	}

	#[test]
	fn rejects_embedded_nuls_with_stable_errors() {
		let cases = [
			(
				intent(os(&[b't' as u16, 0]), Vec::new()),
				"PTY program contains an embedded NUL",
			),
			(
				intent(
					OsString::from("tool"),
					vec![ArgIntent::Regular(os(&[b'a' as u16, 0]))],
				),
				"PTY argument contains an embedded NUL",
			),
			(
				intent(
					OsString::from("tool"),
					vec![ArgIntent::Raw(os(&[b'a' as u16, 0]))],
				),
				"PTY raw argument contains an embedded NUL",
			),
		];

		for (intent, message) in cases {
			let error = prepare_command_line(&intent).unwrap_err();
			assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
			assert_eq!(error.to_string(), message);
		}
	}
}
