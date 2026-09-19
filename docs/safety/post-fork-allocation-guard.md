# Native post-fork allocation guard

Status: draft for a separate effort.

## Goal

Turn allocation or deallocation between `fork` and `exec` into a deterministic native test failure.
This check directly targets the class of regression that allowed tracing inside the historical reset-sigmask callback.

The guard supplements safety proofs.
It cannot detect every post-fork lock, TLS access, or non-allocating library operation.

## Test allocator

Create a dedicated Unix integration-test target with a custom global allocator wrapping `System`.
Before spawning, store the parent PID and arm the guard in atomics initialized without allocation.
Every allocator operation checks the current PID with `getpid`.
If the PID differs while the guard is armed, it calls `_exit` with a reserved status instead of invoking allocator or runtime error machinery.

Guard these operations:

- allocation;
- zeroed allocation;
- reallocation;
- deallocation.

The implementation itself must avoid formatting, panicking, environment access, locks, and allocation on the child failure path.
`getpid` and `_exit` are POSIX async-signal-safe.

## Failure reporting

The executed helper program should return a known success status after `exec`.
The parent must handle both ways an early child `_exit` may surface through the standard process implementation:

- spawn reports the child setup failure;
- spawn succeeds but waiting returns the reserved exit status.

A test passes only when the helper reaches its post-exec success path.
The reserved status must not overlap expected helper outcomes.

## Coverage matrix

Exercise each built-in Unix child policy independently and in valid combinations:

- process-group leadership;
- attachment to an existing process group;
- process-session creation;
- signal-mask reset;
- Tokio PTY child setup;
- signal-mask reset combined with PTY setup.

Run both blocking and Tokio frontends where the policy exists.
Enable tracing and install an active subscriber so any child-side trace regression reaches subscriber machinery rather than being filtered at the macro.
Keep unrelated parent threads active and generate allocator traffic while spawning.

Where practical, force syscall failure paths so raw-OS error construction also executes under the guard.
Failure injection must preserve the child callback's real implementation rather than replacing it with a test copy.

## Isolation

Use a standalone integration-test executable rather than the ordinary parallel test harness for orchestration.
Each case launches one sacrificial child and has an explicit parent-side timeout.
The timeout prevents a separate lock regression from wedging CI, but timeout success is never treated as evidence that allocation was absent.

The allocator belongs only to the test executable.
No production allocator hooks or feature flags are introduced.

## Limitations

The guard does not detect:

- a mutex operation that does not allocate;
- TLS access that is already initialized and lock-free;
- a libc wrapper that uses hidden synchronization without allocating;
- target-specific behavior on kernels where the test does not run;
- invalid external assumptions that happen not to fail during the test.

The check validates the executed paths and toolchain, not all possible implementations.

## Acceptance criteria

- A deliberate allocation inserted into each built-in child callback makes the test fail with the reserved child status.
- Current built-in child setup reaches the executed helper without invoking the guarded allocator.
- Both success and selected syscall-error paths are covered.
- The test runs with tracing enabled and an active subscriber.
- No allocator or guard code is compiled into the library artifact.
