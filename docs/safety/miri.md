# Selective Miri coverage

Status: approved for implementation.

## Goal

Use Miri where it can interpret the real process-wrap code rather than constructing a Miri-specific production implementation.
The initial job covers process-free command state, wrapper lookup, transactional providers, ownership, drop behavior, and public unsafe child traversal.

Miri does not support the process and platform primitives central to `pre_exec`, PTYs, or Windows jobs.
A passing job must not be presented as evidence of async-signal-safety.

## Test configuration

Run with default features disabled and only the blocking frontend enabled:

```sh
cargo +nightly miri test --locked --no-default-features --features std --lib
cargo +nightly miri test --locked --no-default-features --features std --test command_facade
cargo +nightly miri test --locked --no-default-features --features std --test wrapper_lookup
cargo +nightly miri test --locked --no-default-features --features std --test spawn_provider
cargo +nightly miri test --locked --no-default-features --features std --test child_wrapper_contract
```

Use whole test targets rather than name filters.
Libtest considers a filter that matches no tests successful, which would let renamed tests silently remove intended coverage.

## Miri-only exclusions

Mark only cases that actually create a native process with:

```rust
#[cfg_attr(miri, ignore = "requires a native child process")]
```

The initial exclusions are the native fallback case in `spawn_provider` and the native-child cases in `child_wrapper_contract`.
The test bodies continue to compile under Miri.
Synthetic providers, custom children, panic and rollback tests, wrapper lookup, and non-native traversal remain executable.

Do not hide production modules behind `cfg(not(miri))` or add a Miri-specific implementation.

## Covered properties

The selected targets exercise:

- tracked command materialization and transition to native-only state;
- wrapper registry lookup and concrete downcasting;
- provider validation and selection;
- transaction commit and rollback after errors and panics;
- command and wrapper reuse after failed attempts;
- allocation identity and ownership through child-wrapper traversal;
- drop paths for synthetic provider and child state;
- pure Unix dispatcher registration and reuse without invoking its callback.

## Deliberate limitations

The job does not execute or validate:

- `fork`, `exec`, `posix_spawn`, or a `pre_exec` callback;
- session, process-group, signal-mask, or wait syscalls;
- Unix PTY allocation, descriptor passing, or terminal ioctls;
- Tokio process integration;
- Windows processes, handles, job objects, completion ports, or ConPTY;
- target-specific async-signal-safety guarantees.

`-Zmiri-disable-isolation` permits host access for implemented shims but does not implement missing process APIs.
Native-library forwarding executes outside Miri's checks and is not suitable evidence.

## CI job

Install the current nightly toolchain with the Miri component, run `cargo +nightly miri setup`, and execute the whole targets above.
The job is required and must not use `continue-on-error`.
Do not add all features, all targets, native-library forwarding, or isolation disabling.

A moving nightly matches the repository's existing nightly policy.
If the repository later pins nightly, the selected date must first be verified to publish the Miri component.

## Expansion path

The Miri surface can grow when OS-facing functions are separated into small unsafe adapters and pure parsers or state machines.
Candidates are described in [ffi-boundary-extraction.md](ffi-boundary-extraction.md).
Native post-fork checks are described in [post-fork-allocation-guard.md](post-fork-allocation-guard.md) and [post-fork-tripwires.md](post-fork-tripwires.md).

## Acceptance criteria

- The five whole-target commands execute under Miri in CI.
- Only native-process test cases are ignored.
- At least the synthetic provider lifecycle and public unsafe child-traversal cases execute rather than merely compile.
- The workflow and pull-request description state the process and FFI limitations explicitly.
