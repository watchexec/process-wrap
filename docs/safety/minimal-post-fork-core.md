# Minimal post-fork core

Status: draft for a separate effort.

## Goal

Make the built-in child-side setup small enough to compile without `std` or `alloc`, operate only on fixed prepared data, and return raw operating-system error codes.
This creates a mechanically narrower audit boundary while retaining a thin standard-library adapter for `pre_exec`.

A no-`std` component does not by itself prove that every libc function it calls is async-signal-safe.

## Child setup representation

Prepare a fixed-size value in the parent:

```rust
struct ChildSetupPlan {
    reset_sigmask: bool,
    create_session: bool,
    process_group: i32,
}
```

The actual representation may use explicit enums or sentinels, but it must be `Copy`, allocation-free, and fully validated before spawn.
Incompatible session and attached-group states must be rejected while still in the parent.

PTY setup should use a separate fixed plan containing only already-open descriptor numbers and terminal operations required after fork.
No plan may contain owned heap data, mutexes, reference-counted ownership, formatting state, or destructors with child-side work.

## Child executor

The minimal executor:

- accepts a plan by value or immutable reference;
- invokes only audited libc or raw syscall interfaces;
- uses stack storage with explicit initialization;
- reads errno immediately after a failing call;
- returns a compact `PostForkError(i32)`;
- never formats, logs, allocates, panics, unwinds, or consults environment state.

The `pre_exec` adapter converts `PostForkError` to `io::Error::from_raw_os_error` and returns the required `io::Result<()>`.
That adapter remains part of the safety audit because it executes after fork.

## Sharing source with a no-`std` build

A private path dependency would complicate crate publication because Cargo packages cannot rely on an unpublished registry dependency.
Do not create a second publishable crate solely to obtain a `#![no_std]` attribute without first resolving packaging.

A lower-impact option is to keep the executor source in one module and compile that same source through a dedicated no-`std` CI harness.
The harness proves that the shared source requires neither `std` nor `alloc`, while production includes it from the ordinary crate.
Any direct `libc` dependency must be added through Cargo tooling and remain compatible with the crate's MSRV and target matrix.

The design must avoid a test-only copy of the executor because duplicate implementations would not constrain production.

## Generated-code checks

For representative targets, inspect the executor's undefined symbols or reachable call graph.
Permit only the reviewed libc/syscall symbols and compiler intrinsics required by the target.
Fail when allocator, formatting, unwinding, mutex, environment, or thread-runtime symbols become reachable.

Symbol inspection is toolchain- and optimization-sensitive.
Store the rule in terms of forbidden classes plus a small reviewed allowlist, and retain generated output on failure.

## Relationship to the dispatcher

The current reusable native-command dispatcher stores policy in atomics so one installed callback can serve later spawn attempts.
This proposal must preserve command reuse and callback invalidation.

Possible designs are:

- atomically copy policy into a stack `ChildSetupPlan` inside the callback before executing it;
- rematerialize native commands so each spawn captures a plan by value;
- retain the dispatcher but move only syscall execution and raw error handling into the minimal component.

The third option is the smallest architectural change and should be evaluated first.
Atomics used after fork still require a target-specific proof that their implementation is lock-free and does not call runtime helpers.

## Validation

- Compile the shared executor source in the no-`std` harness for every supported target with available standard target metadata.
- Run the native allocation guard against the production adapter.
- Retain native functional tests for session, process-group, sigmask, PTY, and failure behavior.
- Review generated symbols for representative Linux, Apple, BSD, and illumos/Solaris targets.

## Acceptance criteria

- One source implementation is used by production and the no-`std` harness.
- The child executor has no `std`, `alloc`, heap-owning, locking, formatting, or unwinding dependency.
- Parent-side validation produces only representable plans.
- The adapter's raw-OS error conversion is documented and covered by the allocation guard.
- Packaging does not introduce an unpublished runtime dependency.
