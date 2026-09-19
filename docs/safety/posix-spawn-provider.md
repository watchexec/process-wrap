# Native `posix_spawn` provider

Status: draft for a separate effort.

## Goal

Avoid custom post-fork callbacks for Unix child setup that can be expressed through `posix_spawn` attributes and file actions.
The provider should preserve process-wrap's tracked command intent, wrapper lifecycle, transactional cleanup, and child capabilities.

This proposal does not assume that every supported target exposes the complete current POSIX interface through its deployed libc and SDK.

## Expressible setup

Current POSIX spawn facilities can represent:

- child signal masks and selected default signal dispositions;
- new process-group leadership or attachment;
- session creation where `POSIX_SPAWN_SETSID` is available;
- ordered descriptor close, duplicate, and open actions;
- working-directory changes where the target exposes the corresponding file-action extension;
- identity and scheduling attributes supported by the target.

These facilities can replace the built-in reset-sigmask, process-group, and process-session callback for capable targets.

Standard spawn actions do not portably acquire a controlling terminal through `TIOCSCTTY`.
The PTY provider therefore needs a target extension, an existing target-specific primitive, or a retained audited post-fork path.

## Capability matrix

Before implementation, record compile-time and runtime support for each operation on:

- Linux with glibc and musl;
- Android;
- macOS and iOS;
- FreeBSD;
- NetBSD;
- DragonFly BSD;
- OpenBSD;
- illumos and Solaris.

For each target, verify headers, libc symbols, flag semantics, minimum deployment assumptions, and behavior when attributes are combined.
Do not infer availability solely from the current POSIX specification.
Session creation combined with process-group attributes requires particular scrutiny.

## Provider integration

Implement the backend as a Unix spawn provider operating on tracked `SpawnAttempt` state.
It must consume the already validated command intent without reconstructing native-only commands.

The provider must preserve:

- exact program, argument, environment, and cwd semantics;
- configured stdio ownership and close-on-exec behavior;
- wrapper ordering and peer policy;
- `pre_spawn`, provider validation, `post_spawn`, and child wrapping;
- rollback after provider, hook, or wrapping errors and panics;
- blocking and Tokio child contracts;
- process-group and session child supervision after spawn.

Native-only commands and arbitrary user `pre_exec` callbacks must continue to use native spawning or return an explicit unsupported-provider error.
The provider must never silently discard an escape hatch.

## Error transport

`posix_spawn` returns an error number to the parent and does not require process-wrap to construct errors in a custom child callback.
Convert the returned integer directly to `io::Error::from_raw_os_error` in the parent.

File-action construction and attribute setup occur in the parent and may use ordinary Rust error handling.
Partially constructed actions and attributes require RAII cleanup before spawn.

## Selection policy

Prefer the provider only when all requested command and wrapper policies are representable on the target.
Fall back to the existing native path for unsupported combinations unless provider selection was explicit.
An explicit provider request should return a precise capability error rather than changing semantics.

Keep selection deterministic and test it as a pure capability decision.

## PTY path

Treat PTY controlling-terminal acquisition as a separate capability.
Targets with a documented spawn extension may use it after independent verification.
Other targets retain the current PTY child setup until another design replaces it.
Do not claim that the ordinary provider eliminates all process-wrap post-fork code while this fallback exists.

## Validation

- Unit-test capability selection and spawn-action construction without creating a process.
- Compare native and provider command/environment behavior using the existing command-facade fixtures.
- Run process-group, session, sigmask, stdio, error, and child-lifecycle integration tests on each native target.
- Verify that unsupported and native-only attempts fail before external side effects.
- Retain the allocation guard on any fallback callback.

## Acceptance criteria

- Capable non-PTY configurations spawn without registering a process-wrap `pre_exec` callback.
- Unsupported combinations are explicit and never lose command or wrapper policy.
- Provider and native paths produce equivalent observable command and child behavior for their shared feature set.
- The target capability matrix cites the actual libc and SDK interfaces used by the crate.
- Documentation states which paths still require post-fork setup.
