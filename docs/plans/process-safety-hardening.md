# Process Safety Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the current unsafe-code proof baseline and add enforced Clippy and selective Miri coverage without changing library runtime behavior or API shape.

**Architecture:** Treat comments and caller-facing contracts as part of each unsafe boundary, enforce their presence through package lints across native and portable cfg paths, and interpret only process-free targets under Miri. Keep runtime repairs, public API redesigns, and private safety-signature changes in the permanent future-work drafts.

**Tech Stack:** Rust 1.87.0 MSRV, Cargo manifest lints, Clippy, nightly Miri, GitHub Actions, Jujutsu.

**Specs:** `docs/safety/unsafe-hardening.md`, `docs/safety/miri.md`, and `docs/safety/audit-disposition.md`.

## Global Constraints

- Do not add `unsafe_code = "deny"` or per-site `allow(unsafe_code)`/`expect(unsafe_code)` annotations.
- Do not change runtime behavior, public API signatures, private function safety signatures, or wrapper semantics.
- Do not remove parent-side tracing from reset-sigmask or process-session configuration.
- Do not claim that Miri executes process creation, FFI, PTY, Windows, or post-`fork` paths.
- Do not add Miri-specific production implementations, disabled isolation, native-library forwarding, `continue-on-error`, or name-filter-only Miri commands.
- Use `jj` for commits and leave each independently reviewable change in its own commit.
- Do not comment on issue #56 or tag its author in commits or the pull request.

---

### Task 1: Establish lint policy and complete public unsafe contracts

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/command.rs`

**Interfaces:**
- Consumes: the existing `NativeCommand`, `Command`, and `SpawnAttempt` unsafe callback APIs.
- Produces: package-wide lint enforcement and complete caller contracts used by every later Clippy task.

- [ ] Add the package lint tables exactly as follows:

```toml
[lints.rust]
unsafe_op_in_unsafe_fn = "deny"
unused_unsafe = "deny"

[lints.clippy]
missing_safety_doc = "deny"
undocumented_unsafe_blocks = "deny"
```

- [ ] Add a `# Safety` section to `NativeCommand::pre_exec` covering the post-`fork`, pre-`exec` environment, allocation, lock acquisition, environment access, formatting, and target async-signal-safety obligations.
- [ ] Replace adapter comments that merely say the caller accepts the native contract with comments identifying the documented trait contract being forwarded unchanged.
- [ ] Expand both `SpawnAttempt::pre_exec` contracts to match the command-level warning about allocation and lock acquisition.
- [ ] Commit as `docs(safety): define unsafe API contracts`.

### Task 2: Complete central Unix and standard child-I/O proofs

**Files:**
- Modify: `src/unix.rs`
- Modify: `src/std/core.rs`
- Modify if Clippy identifies an incomplete existing boundary: `src/tokio/core.rs`

**Interfaces:**
- Consumes: the centralized atomic Unix dispatcher and standard dual-pipe reader.
- Produces: local proofs for post-fork policy reads, raw OS setup calls, borrowed descriptors, and Linux nonblocking configuration.

- [ ] Expand the dispatcher registration rationale to cover the captured `Arc` values, lock-free atomic access expected on supported targets, prepared policy ordering, absence of callback mutex access, and raw-OS error paths.
- [ ] Refine `reset_sigmask`, `setsid`, and `setpgid` comments so each proof covers initialized storage, pointer lifetime where applicable, POSIX-listed operations, errno retrieval, and raw-OS error construction without claiming that arbitrary `io::Error` creation is safe.
- [ ] Replace the standard reader's drop-timing comment with a lifetime proof showing that `ChildStdout` and `ChildStderr` own the descriptors until every `BorrowedFd` and `PollFd` use ends, including early returns.
- [ ] Add the Linux `FIONBIO` proof covering the live borrowed descriptor, request value, aligned `c_int` argument, and call duration.
- [ ] Retain the existing Tokio borrowed-handle proof if it already satisfies the lint and ownership invariant; strengthen it only if review identifies a missing premise.
- [ ] Commit as `docs(safety): prove Unix process boundaries`.

### Task 3: Complete Windows handle and job-object proofs

**Files:**
- Modify: `src/windows.rs`
- Modify: `src/std/job_object.rs`
- Modify: `src/tokio/job_object.rs`

**Interfaces:**
- Consumes: current owned raw handles, job/completion-port creation, thread snapshot fallback, termination, waiting, and unsafe wrapper removal.
- Produces: local ownership, layout, pointer, transfer, and close proofs without changing the completion algorithm or leak behavior.

- [ ] Document `OwnedHandle::drop` and `JobPort::drop` with successful-result ownership, one-time destruction, and distinct-handle premises.
- [ ] Add complete `Send` and `Sync` rationales for `JobHandle` and `PortHandle`, including private ownership and Rust borrowing that prevent close during borrowed use.
- [ ] Split job-object creation and configuration into adjacent proofs for each Win32 call, covering valid handles, exact structure types, initialized pointers, byte lengths, and call-scoped borrows.
- [ ] Document snapshot creation, process-ID lookup, thread enumeration, thread opening/resume, termination, and completion dequeue with each output buffer's validity and initialization rules.
- [ ] Document the standard-library and Tokio unsafe unwrap close operations, including `ManuallyDrop`, completion-port ownership, and deliberate finalized kill-on-close job-handle retention.
- [ ] Do not repair packet filtering, timeout handling, cached completion state, or handle retention in this task.
- [ ] Commit as `docs(safety): prove Windows handle boundaries`.

### Task 4: Correct and complete Unix PTY and descriptor-transfer proofs

**Files:**
- Modify: `src/tokio/pty/unix.rs`
- Modify: `src/tokio/pty/unix/descriptor_pair.rs`
- Modify: `src/tokio/pty/unix/openbsd.rs`

**Interfaces:**
- Consumes: current PTY allocation, child setup, descriptor passing, and OpenBSD helper implementations.
- Produces: precise platform assumptions and complete contracts while preserving every call and signature.

- [ ] Split the PTY child setup rationale so POSIX-listed `setsid`, `getpgrp`, and `tcsetpgrp` operations are not used to imply that `ioctl(TIOCSCTTY)` is mandated async-signal-safe.
- [ ] Record the narrower supported-target assumption that the controlling-terminal ioctl uses the target libc's direct syscall-oriented boundary without allocation or locks.
- [ ] Add or strengthen local descriptor, pointer, buffer-length, initialization, and ownership proofs found incomplete by Clippy in PTY allocation and I/O helpers.
- [ ] Add a full `# Safety` contract to the existing unsafe OpenBSD helper without changing its signature, covering inherited descriptors, post-fork child-only execution, syscall-oriented operations, no unwind, and `_exit` termination.
- [ ] Review every `CMSG_*`, `recvmsg`, `sendmsg`, raw descriptor conversion, and ancillary copy rationale against its live storage and exact bounds.
- [ ] Leave the safe raw-pointer parser and safe `msghdr` decoder signatures unchanged for the separate FFI-boundary effort.
- [ ] Commit as `docs(safety): prove PTY and descriptor boundaries`.

### Task 5: Complete unsafe test rationales

**Files:**
- Modify: `tests/command_facade.rs`
- Modify: `tests/child_wrapper_contract.rs`
- Modify: `tests/std_unix/into_inner_write_stdin.rs`
- Modify: `tests/std_windows/process_handle.rs`
- Modify: `tests/support/windows_thread.rs`
- Modify: `tests/tokio_pty_provider.rs`
- Modify: `tests/tokio_pty_unix.rs`
- Modify: `tests/tokio_windows/job_object_kill_on_drop.rs`
- Modify: `tests/tokio_windows/process_handle.rs`

**Interfaces:**
- Consumes: fixtures that establish native-child, raw-handle, descriptor, process, and wrapper-layer preconditions.
- Produces: test-local proofs that explain exactly how each fixture satisfies the unsafe API being exercised.

- [ ] Add or strengthen comments for unsafe command facade and child traversal calls, naming the wrapper invariants the test intentionally permits bypassing.
- [ ] Document ownership transfer for Unix child stdin extraction and PTY raw process/wait operations.
- [ ] Document Windows borrowed process handles, snapshot/thread operations, suspension/resume, completion, and close ownership in test support and process-handle tests.
- [ ] Document job-object kill-on-drop test calls with the handle and lifecycle state established by the fixture.
- [ ] Keep test comments focused on the immediate unsafe precondition rather than restating expected assertions.
- [ ] Commit as `docs(safety): prove unsafe test fixtures`.

### Task 6: Enforce safety lints across native and portable configurations

**Files:**
- Modify: `.github/workflows/test.yml`

**Interfaces:**
- Consumes: the package lint policy from Task 1 and the existing native and portable target matrices.
- Produces: required CI jobs that compile every relevant feature and platform path under Clippy.

- [ ] Add `--all-targets` to the existing frontend-matrix Clippy command.
- [ ] Install Clippy in the native all-features jobs and add `cargo clippy --locked --all-features --all-targets`.
- [ ] Add one Ubuntu job using Rust 1.87.0 and `cargo clippy --locked --all-features --all-targets`.
- [ ] Install Clippy in the portable Unix PTY jobs.
- [ ] Replace stable portable `cargo check` with `cargo clippy --locked --target "${{ matrix.target }}" --no-default-features --features pty --lib`.
- [ ] Replace nightly portable `cargo check` with the same Clippy command retaining `-Zbuild-std=std`.
- [ ] Leave rustdoc, doctest, package, semver, checkout, runner, and release behavior unchanged.
- [ ] Commit as `ci: enforce unsafe boundary lints`.

### Task 7: Add selective Miri coverage

**Files:**
- Modify: `tests/spawn_provider.rs`
- Modify: `tests/child_wrapper_contract.rs`
- Modify: `.github/workflows/test.yml`

**Interfaces:**
- Consumes: existing synthetic provider, command facade, wrapper lookup, child traversal, and pure Unix dispatcher tests.
- Produces: one required Ubuntu Miri job over whole process-free targets.

- [ ] Add `#[cfg_attr(miri, ignore = "requires a native child process")]` to `native_fallback_runs_the_complete_lifecycle` in `tests/spawn_provider.rs`.
- [ ] Add the same Miri-only ignore to both blocking native-child cases and both Tokio native-child cases in `tests/child_wrapper_contract.rs`, keeping their bodies compiled.
- [ ] Add an Ubuntu `miri` job that installs current nightly with the Miri component and runs the exact process-free commands below:

```sh
cargo +nightly miri setup
cargo +nightly miri test --locked --no-default-features --features std --lib
cargo +nightly miri test --locked --no-default-features --features std --test command_facade
cargo +nightly miri test --locked --no-default-features --features std --test wrapper_lookup
cargo +nightly miri test --locked --no-default-features --features std --test spawn_provider
cargo +nightly miri test --locked --no-default-features --features std --test child_wrapper_contract
```

- [ ] Keep the job required and omit `continue-on-error`, all-features, all-targets, name filters, isolation disabling, and native-library forwarding.
- [ ] Name or comment the job so its process-free scope is explicit.
- [ ] Commit as `ci: add process-free Miri coverage`.

### Task 8: Reconcile implementation and remove the temporary plan

**Files:**
- Review: `docs/safety/unsafe-hardening.md`
- Review: `docs/safety/miri.md`
- Review: `docs/safety/audit-disposition.md`
- Delete: `docs/plans/process-safety-hardening.md`

**Interfaces:**
- Consumes: every implementation commit above.
- Produces: a stack with all approved implementation complete and no stale temporary plan.

- [ ] Compare the implemented files against every approved requirement in the three specs.
- [ ] Resolve every implementation omission or explicitly stop for a user decision if a requirement cannot be met without runtime/API scope.
- [ ] Delete this plan only after all implementation items are complete.
- [ ] Commit the deletion as `unplan: process safety hardening`.
- [ ] Create or move the hardening bookmark to the completed tip and leave a new empty undescribed working commit.
