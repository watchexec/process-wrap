//! Exact Win32 process-spawn intent preparation.
//!
//! This private model consumes tracked command state directly. It retains the WTF-16 data needed by
//! the ConPTY backend's `CreateProcessW` call without committing those implementation invariants to
//! the public API.

pub(super) mod command;
pub(super) mod environment;
