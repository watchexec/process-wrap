# Native post-fork tripwires

Status: draft for a separate effort.

## Goal

Add Linux-native regression checks for post-fork tracing, lock acquisition, and unexpected syscalls that an allocation guard cannot observe.
These checks provide evidence for specific failure classes and are not a portable async-signal-safety proof.

## Tracing-subscriber lock tripwire

Build a test subscriber that records the parent PID and owns a mutex.
A background parent thread acquires that mutex before spawn and holds it across the fork boundary.
Parent-side events bypass the held mutex after checking that their PID matches the recorded parent.
An event observed under a different PID attempts the inherited held lock and therefore cannot complete.

Spawn runs under a parent-side watchdog.
A timeout identifies child-side tracing or equivalent subscriber entry.
A separate sentinel path should distinguish an expected child setup error from a wedged callback.

This design deliberately reproduces the lock state that makes logging after a multithreaded fork unsafe.
It only detects entry into the configured subscriber and does not cover arbitrary locks elsewhere.

## Direct event tripwire

A simpler companion subscriber can call `_exit` with a reserved status if any event is delivered under a PID different from the parent.
This detects child-side events without relying on an actual deadlock and provides a clearer failure code.
The subscriber's child branch must use only PID comparison and `_exit`.

Run both subscriber variants because the direct tripwire verifies event delivery while the lock variant reproduces the original deadlock mechanism.

## Syscall observation

A Linux-only test can trace the sacrificial child and compare the post-fork, pre-exec syscall sequence against an allowlist derived from the selected policy.
Expected calls include the process-group, session, signal-mask, PTY, descriptor, exec, and error-channel operations required by that case.
Unexpected `futex`, memory-mapping, filesystem, networking, or process-management calls should fail the test or require an explicit reviewed allowance.

The trace must identify the child by PID and stop comparison at successful `exec`.
It must retain the raw trace as an artifact when the allowlist comparison fails.

A seccomp variant may enforce an allowlist rather than observe it, but should only be considered after the trace establishes the standard library's required child syscalls.
The filter itself must be installed before the library callback without changing callback ordering or implementation.

## Why syscall checks are supplementary

Allocation can be satisfied from an inherited arena without `mmap` or `brk`.
An uncontended lock can complete without `futex`.
Libc and standard-library implementations may change their internal syscall sequence while remaining valid.
For those reasons, syscall absence does not prove allocation or lock absence.

The allocation guard in [post-fork-allocation-guard.md](post-fork-allocation-guard.md) remains the direct allocation regression check.

## Portability

Keep these checks Linux-specific.
Do not infer that a Linux libc syscall sequence proves behavior on Apple, BSD, illumos, Solaris, or Android targets.
Native target execution is covered separately in [native-platform-ci.md](native-platform-ci.md).

## Acceptance criteria

- Reintroducing a child-side tracing event fails both subscriber tripwires deterministically.
- Every case has a watchdog that terminates the test process tree on a wedge.
- Syscall comparison stops at `exec` and emits an inspectable trace on mismatch.
- Allowlist updates require a rationale tied to the child policy that needs the syscall.
- CI and documentation describe these as regression checks rather than formal proof.
