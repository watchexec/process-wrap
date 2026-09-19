# Unsafe boundary hardening

Status: approved for implementation.

## Goal

Make every current unsafe operation reviewable at its point of use and ensure CI compiles the configurations containing those operations.
This work changes comments, documentation, lint policy, and CI only.
It does not change runtime behavior, public API shape, or private safety signatures.

The historical `trace!`-after-`fork` defect is no longer present on `main`.
Reset-sigmask and process-session wrappers now record policy in the parent, and `src/unix.rs` installs the centralized native child callback.

## Lint policy

Add package-level denies for:

```toml
[lints.rust]
unsafe_op_in_unsafe_fn = "deny"
unused_unsafe = "deny"

[lints.clippy]
missing_safety_doc = "deny"
undocumented_unsafe_blocks = "deny"
```

Do not pair `unsafe_code = "deny"` with an exemption at every intentional unsafe site.
Such exemptions duplicate the `unsafe` syntax without defining an architectural boundary.

The lints verify that a rationale exists and that unsafe function bodies retain explicit blocks.
They cannot verify that a rationale is correct.

## Proof standard

A `SAFETY` rationale must state the invariant that makes the operation valid rather than restating the callee or saying that the caller accepts its contract.
Depending on the operation, it must cover:

- pointer provenance, initialized range, alignment, and call duration;
- descriptor or handle validity, ownership, and close behavior;
- aliasing and borrowing for raw descriptor views;
- exact structure layout and buffer size supplied to an operating-system API;
- thread transfer and concurrent-use invariants for unsafe `Send` and `Sync` implementations;
- post-`fork` restrictions, including the complete success and error paths;
- test setup that establishes the preconditions of public unsafe traversal APIs.

Access-right failures and absent kernel objects are ordinary API errors, not memory-safety proofs.

## Source coverage

### Public and shared command APIs

Review `src/command.rs` for the native command abstraction and all public `pre_exec` escape hatches.
Add the complete post-`fork` contract to `NativeCommand::pre_exec`.
Align both `SpawnAttempt::pre_exec` variants with the stronger command-level documentation, including the prohibition on allocation and lock acquisition.
Retain explicit blocks in the standard-library and Tokio adapters.

### Central Unix child setup

Review `src/unix.rs` as one proof boundary.
The callback may read only policy atomics populated before spawn and must not access the parent-side callback mutex.
The proof must cover signal-mask initialization, `pthread_sigmask`, session and process-group syscalls, errno access, raw-OS error construction, and the target assumptions behind atomic operations after `fork`.

### Standard child I/O

Strengthen the borrowed-descriptor lifetime proof in `src/std/core.rs`.
The owned stdout and stderr values must remain alive while `BorrowedFd` and `PollFd` values exist, including early returns.
Document the Linux `FIONBIO` request, descriptor validity, argument layout, and call lifetime.

### Windows job objects and handles

Add local rationales throughout `src/windows.rs` and the standard-library and Tokio job-object unwrap paths.
Cover successful-result ownership, one-time close behavior, distinct job and completion-port handles, exact job-information layouts, snapshot and thread-entry buffers, and completion-status output pointers.

The `Send` and `Sync` proofs for handle wrappers must include the private ownership model that prevents concurrent close while a borrowed operation is in progress.
Saying only that kernel handles are thread-safe is insufficient.

### Unix PTY and descriptor transfer

Review `src/tokio/pty/unix.rs`, its target modules, and `descriptor_pair.rs`.
Preserve existing precise descriptor, buffer, and ownership comments.
Add missing contracts and correct any claim that every PTY child operation is required by POSIX to be async-signal-safe.
In particular, controlling-terminal `ioctl` calls rely on supported-target libc behavior beyond POSIX's mandatory function list.

The OpenBSD helper must document that it runs only in the post-fork child, uses syscall-oriented operations, never unwinds, and terminates through `_exit` without Rust teardown.

### Tests

Review unsafe calls in command-facade, child-wrapper, Unix child, PTY provider, PTY Unix, Windows process-handle, job-object, and Windows thread-support tests.
Each rationale must identify the fixture or owned value that establishes the unsafe API's precondition.

## CI enforcement

Run Clippy over all targets in the existing frontend matrix and over all features on native Linux, macOS, and Windows.
Run all-feature Clippy at the declared MSRV on Ubuntu.
Run Clippy for the portable Unix PTY library targets, retaining `-Zbuild-std=std` for targets that already require it.

Selective Miri coverage is specified separately in [miri.md](miri.md).

## Non-goals

This effort does not:

- claim that comments prove POSIX or Win32 behavior;
- add unsafe-code counters;
- add broad warning denial unrelated to the selected safety lints;
- change safe functions into unsafe functions;
- repair runtime defects found during the audit;
- remove safe parent-side tracing;
- claim Miri coverage of process creation, FFI, or async-signal-safety.

## Acceptance criteria

- Every compiled unsafe block and unsafe implementation has a reviewed local rationale.
- Every public unsafe function has a caller-facing `# Safety` contract.
- Unsafe operations inside unsafe functions remain in explicit blocks.
- Native, portable-target, and MSRV Clippy jobs compile the relevant feature and platform paths.
- Existing comments that overstate POSIX guarantees are corrected rather than mechanically retained.
