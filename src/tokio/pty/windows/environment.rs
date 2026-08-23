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
