# Native platform CI

Status: draft for a separate effort.

## Goal

Execute target-specific PTY, descriptor, process, and handle behavior on the kernels that define it.
Cross-compilation remains useful for cfg, type, layout, and symbol coverage but cannot validate runtime FFI contracts.

## Existing baseline

The repository already executes native jobs on Linux, macOS, and Windows.
It cross-checks PTY library builds for Android, iOS, FreeBSD, NetBSD, illumos, Solaris, DragonFly BSD, and OpenBSD.
Those checks should remain even when native jobs are added.

## Target priorities

### OpenBSD

OpenBSD has a unique PTY allocation helper using `fork`, `/dev/ptm`, `PTMGET`, `SCM_RIGHTS`, and `_exit`.
Native execution should cover descriptor transfer, close-on-exec installation, helper reaping, invalid responses, terminal sizing, and repeated allocation.

### FreeBSD and other BSDs

Run ordinary PTY allocation, controlling-terminal setup, process-group/session interaction, signal-mask reset, resize, I/O, EOF, and cleanup tests.
FreeBSD provides an initial second kernel family before adding NetBSD and DragonFly BSD.

### illumos and Solaris

Exercise terminal ioctls, descriptor flags, process/session policy, and EOF behavior on the System V-derived targets.
Keep target-specific expectations separate rather than treating the two names as identical environments.

### Android and iOS

Retain compile checks unless a supported emulator or device environment permits the process and PTY semantics used by the crate.
Mobile sandbox restrictions can make an emulator result about application policy rather than the underlying API.
Document any tests intentionally unavailable there.

### Windows

Extend the existing native suite with handle-count and completion-port message-order tests.
Use operating-system diagnostics or verifier tooling only when it can run without weakening untrusted-pull-request isolation.

## CI hosting

Evaluate native virtual-machine jobs, project-controlled runners, and external CI services against these requirements:

- reproducible operating-system image or documented rolling policy;
- no repository secrets exposed to untrusted pull requests;
- least-privilege tokens and read-only checkout;
- inspectable bootstrap scripts;
- pinned third-party actions or images where pinning is available;
- artifact capture for kernel, libc, architecture, and failing process traces;
- cancellation that terminates descendant process trees.

Do not add an opaque third-party action solely to obtain a target label.
The execution environment is part of the FFI evidence and must be reviewable.

## Test organization

Keep portable behavioral contracts shared across targets.
Place only genuine target differences behind cfg-specific fixtures or assertions.

Native jobs should cover:

- command and environment preservation;
- process group, session, and signal-mask policy;
- PTY allocation, controlling terminal, foreground group, resize, and merged I/O;
- close-on-exec and descriptor ownership;
- cancellation, rollback, and process-tree cleanup;
- target-specific helper protocols;
- Windows job assignment, suspension/resume, completion packets, and handle lifetime.

Every process test needs a parent-side timeout and cleanup path.
A failure must retain enough process and platform information to distinguish a test defect from an operating-system semantic difference.

## Relationship to other checks

Miri covers pure Rust state and ownership but not these APIs.
The allocation guard detects one post-fork failure class on executed Unix hosts.
Portable Clippy checks comments and cfg-specific code without running it.
Native CI supplies runtime evidence and does not replace any of those layers.

## Rollout

Add one native target at a time with the tests most specific to that target.
Start with OpenBSD because its helper architecture is otherwise never executed, then add FreeBSD, followed by the illumos/Solaris and remaining BSD targets according to available controlled runners.

Each addition must document which existing cross-check it complements and which runtime paths remain uncovered.

## Acceptance criteria

- At least one non-Linux, non-Apple Unix target executes PTY and child-policy tests on its native kernel.
- OpenBSD-specific helper code executes natively before its compile-only status is considered resolved.
- Jobs isolate untrusted pull requests from credentials and persistent runner state.
- Failures retain kernel, libc, architecture, and relevant process diagnostics.
- Documentation distinguishes compile coverage, interpreter coverage, and native behavioral coverage.
