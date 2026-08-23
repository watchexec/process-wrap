use std::{cmp::Ordering, io, slice};

use windows::{
	Win32::{
		Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
		System::Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW},
	},
	core::PWSTR,
};

use super::{
	super::{EnvChange, EnvironmentIntent},
	command::encode,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct EnvironmentVariable {
	key: Vec<u16>,
	value: Vec<u16>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum PreparedEnvironment {
	Inherit,
	Block(Vec<u16>),
}

impl PreparedEnvironment {
	pub(super) fn as_ptr(&self) -> *const u16 {
		match self {
			Self::Inherit => std::ptr::null(),
			Self::Block(block) => block.as_ptr(),
		}
	}

	pub(super) fn is_inherited(&self) -> bool {
		matches!(self, Self::Inherit)
	}
}

pub(super) fn prepare_environment(intent: &EnvironmentIntent) -> io::Result<PreparedEnvironment> {
	prepare_environment_with(intent, inherited_environment)
}

fn prepare_environment_with(
	intent: &EnvironmentIntent,
	capture: impl FnOnce() -> io::Result<Vec<EnvironmentVariable>>,
) -> io::Result<PreparedEnvironment> {
	let changes = intent
		.changes
		.iter()
		.map(|change| match change {
			EnvChange::Set(key, value) => Ok(EncodedChange::Set(EnvironmentVariable {
				key: encode(key, "PTY environment key")?,
				value: encode(value, "PTY environment value")?,
			})),
			EnvChange::Remove(key) => {
				Ok(EncodedChange::Remove(encode(key, "PTY environment key")?))
			}
		})
		.collect::<io::Result<Vec<_>>>()?;

	if !intent.clear && changes.is_empty() {
		return Ok(PreparedEnvironment::Inherit);
	}

	let mut variables = Vec::new();
	if !intent.clear {
		for variable in capture()? {
			set_variable(&mut variables, variable);
		}
	}
	for change in changes {
		match change {
			EncodedChange::Set(variable) => set_variable(&mut variables, variable),
			EncodedChange::Remove(key) => {
				variables.retain(|variable| !windows_equal(&variable.key, &key));
			}
		}
	}
	variables.sort_by(|left, right| windows_compare(&left.key, &right.key));

	let mut block = Vec::new();
	for variable in variables {
		block.extend(variable.key);
		block.push(b'=' as u16);
		block.extend(variable.value);
		block.push(0);
	}
	block.push(0);
	if block.len() == 1 {
		block.push(0);
	}
	Ok(PreparedEnvironment::Block(block))
}

#[derive(Debug)]
enum EncodedChange {
	Set(EnvironmentVariable),
	Remove(Vec<u16>),
}

fn set_variable(variables: &mut Vec<EnvironmentVariable>, variable: EnvironmentVariable) {
	if let Some(existing) = variables
		.iter_mut()
		.find(|existing| windows_equal(&existing.key, &variable.key))
	{
		*existing = variable;
	} else {
		variables.push(variable);
	}
}

fn windows_equal(left: &[u16], right: &[u16]) -> bool {
	windows_compare(left, right) == Ordering::Equal
}

fn windows_compare(left: &[u16], right: &[u16]) -> Ordering {
	// SAFETY: both slices remain live for the call and CompareStringOrdinal accepts explicit lengths.
	match unsafe { CompareStringOrdinal(left, right, true) } {
		CSTR_LESS_THAN => Ordering::Less,
		CSTR_EQUAL => Ordering::Equal,
		CSTR_GREATER_THAN => Ordering::Greater,
		_ => left.cmp(right),
	}
}

fn inherited_environment() -> io::Result<Vec<EnvironmentVariable>> {
	// SAFETY: ownership of a successful environment block is immediately placed in a drop guard.
	let block = unsafe { GetEnvironmentStringsW() };
	if block.is_null() {
		return Err(io::Error::last_os_error());
	}
	let block = EnvironmentBlock(block);
	parse_environment_block(block.0)
}

struct EnvironmentBlock(PWSTR);

impl Drop for EnvironmentBlock {
	fn drop(&mut self) {
		// SAFETY: this is the same non-null pointer returned by GetEnvironmentStringsW.
		let _ = unsafe { FreeEnvironmentStringsW(self.0) };
	}
}

fn parse_environment_block(block: PWSTR) -> io::Result<Vec<EnvironmentVariable>> {
	let mut variables = Vec::new();
	let mut cursor = block.0;
	loop {
		let mut len = 0;
		// SAFETY: GetEnvironmentStringsW returns a double-NUL-terminated sequence.
		while unsafe { *cursor.add(len) } != 0 {
			len += 1;
		}
		if len == 0 {
			break;
		}
		// SAFETY: the scan above established that this entry contains len initialized units.
		let entry = unsafe { slice::from_raw_parts(cursor, len) };
		variables.push(parse_environment_entry(entry)?);
		// SAFETY: advance over the entry and its terminating NUL within the environment block.
		cursor = unsafe { cursor.add(len + 1) };
	}
	Ok(variables)
}

fn parse_environment_entry(entry: &[u16]) -> io::Result<EnvironmentVariable> {
	let separator = if entry.first() == Some(&(b'=' as u16)) {
		entry[1..]
			.iter()
			.position(|unit| *unit == b'=' as u16)
			.map(|position| position + 1)
	} else {
		entry.iter().position(|unit| *unit == b'=' as u16)
	}
	.ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidData,
			"Windows environment entry has no key-value separator",
		)
	})?;

	Ok(EnvironmentVariable {
		key: entry[..separator].to_vec(),
		value: entry[separator + 1..].to_vec(),
	})
}

#[cfg(test)]
mod tests {
	use std::{
		ffi::{OsStr, OsString},
		os::windows::ffi::{OsStrExt, OsStringExt},
	};

	use windows::core::PWSTR;

	use super::*;

	fn units(value: &str) -> Vec<u16> {
		OsStr::new(value).encode_wide().collect()
	}

	fn os(units: &[u16]) -> OsString {
		OsString::from_wide(units)
	}

	fn variable(key: &str, value: &str) -> EnvironmentVariable {
		EnvironmentVariable {
			key: units(key),
			value: units(value),
		}
	}

	fn set(key: &str, value: &str) -> EnvChange {
		EnvChange::Set(OsString::from(key), OsString::from(value))
	}

	fn remove(key: &str) -> EnvChange {
		EnvChange::Remove(OsString::from(key))
	}

	fn intent(clear: bool, changes: Vec<EnvChange>) -> EnvironmentIntent {
		EnvironmentIntent { clear, changes }
	}

	fn block(entries: &[(&str, &str)]) -> PreparedEnvironment {
		let mut block = Vec::new();
		for (key, value) in entries {
			block.extend(units(key));
			block.push(b'=' as u16);
			block.extend(units(value));
			block.push(0);
		}
		block.push(0);
		if block.len() == 1 {
			block.push(0);
		}
		PreparedEnvironment::Block(block)
	}

	#[test]
	fn inherits_without_capturing_an_unchanged_environment() {
		let prepared = prepare_environment_with(&intent(false, Vec::new()), || {
			panic!("unchanged environment must not be captured")
		})
		.unwrap();
		assert!(prepared.is_inherited());
		assert!(prepared.as_ptr().is_null());
	}

	#[test]
	fn clearing_to_an_empty_environment_does_not_capture_the_parent() {
		let prepared = prepare_environment_with(&intent(true, Vec::new()), || {
			panic!("cleared environment must not be captured")
		})
		.unwrap();
		assert_eq!(prepared, PreparedEnvironment::Block(vec![0, 0]));
	}

	#[test]
	fn applies_case_insensitive_changes_and_sorts_deterministically() {
		let parent = vec![
			variable("Path", "parent"),
			variable("KEEP", "one"),
			variable("remove", "gone"),
			variable("=C:", r"C:\work"),
		];
		let changes = vec![
			set("PATH", "first"),
			set("path", "second"),
			remove("ReMoVe"),
			set("Alpha", "a"),
			remove("missing"),
		];

		let first = prepare_environment_with(&intent(false, changes), || Ok(parent)).unwrap();
		assert_eq!(
			first,
			block(&[
				("=C:", r"C:\work"),
				("Alpha", "a"),
				("KEEP", "one"),
				("path", "second"),
			])
		);

		let second = prepare_environment_with(
			&intent(
				false,
				vec![
					set("PATH", "first"),
					set("path", "second"),
					remove("ReMoVe"),
					set("Alpha", "a"),
					remove("missing"),
				],
			),
			|| {
				Ok(vec![
					variable("Path", "parent"),
					variable("KEEP", "one"),
					variable("remove", "gone"),
					variable("=C:", r"C:\work"),
				])
			},
		)
		.unwrap();
		assert_eq!(first, second);
	}

	#[test]
	fn clear_discards_the_parent_before_applying_changes() {
		let prepared = prepare_environment_with(&intent(true, vec![set("Only", "value")]), || {
			panic!("cleared environment must not be captured")
		})
		.unwrap();
		assert_eq!(prepared, block(&[("Only", "value")]));
	}

	#[test]
	fn normalizes_case_collisions_in_the_inherited_environment() {
		let prepared = prepare_environment_with(&intent(false, vec![remove("absent")]), || {
			Ok(vec![variable("Name", "first"), variable("NAME", "second")])
		})
		.unwrap();
		assert_eq!(prepared, block(&[("NAME", "second")]));
	}

	#[test]
	fn preserves_lone_surrogates() {
		let key = os(&[b'K' as u16, 0xd800]);
		let value = os(&[b'V' as u16, 0xdc00]);
		let prepared =
			prepare_environment_with(&intent(false, vec![EnvChange::Set(key, value)]), || {
				Ok(Vec::new())
			})
			.unwrap();
		assert_eq!(
			prepared,
			PreparedEnvironment::Block(vec![
				b'K' as u16,
				0xd800,
				b'=' as u16,
				b'V' as u16,
				0xdc00,
				0,
				0,
			])
		);
	}

	#[test]
	fn parses_drive_current_directory_variables() {
		let mut raw = units(r"=C:=C:\work");
		raw.push(0);
		raw.extend(units("Name=value"));
		raw.extend([0, 0]);
		let parsed = parse_environment_block(PWSTR(raw.as_mut_ptr())).unwrap();
		assert_eq!(
			parsed,
			vec![variable("=C:", r"C:\work"), variable("Name", "value")]
		);
	}

	#[test]
	fn rejects_environment_nuls_before_capturing_the_parent() {
		let cases = [
			(
				intent(
					false,
					vec![EnvChange::Set(os(&[b'K' as u16, 0]), OsString::from("v"))],
				),
				"PTY environment key contains an embedded NUL",
			),
			(
				intent(
					false,
					vec![EnvChange::Set(OsString::from("K"), os(&[b'V' as u16, 0]))],
				),
				"PTY environment value contains an embedded NUL",
			),
			(
				intent(false, vec![EnvChange::Remove(os(&[b'K' as u16, 0]))]),
				"PTY environment key contains an embedded NUL",
			),
		];

		for (intent, message) in cases {
			let error = prepare_environment_with(&intent, || {
				panic!("invalid explicit environment must be rejected before capture")
			})
			.unwrap_err();
			assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
			assert_eq!(error.to_string(), message);
		}
	}
}
