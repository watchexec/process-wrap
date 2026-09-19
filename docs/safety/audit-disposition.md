# Audit finding disposition

Status: current evaluation against the command/provider refactor on `main`.

## Historical report claims

### Built-in tracing after `fork`

The reported reset-sigmask defect was valid in the older implementation but is no longer present.
The standard-library and Tokio wrappers now trace while recording policy in parent-side `pre_spawn` hooks.
The centralized callback in `src/unix.rs` performs the child setup without tracing or parent mutex access.

Process-session error conversion also moved out of the wrapper callback.
Current `Error::new` and `Error::other` calls in the process-session wrappers run while configuring or wrapping in the parent.

### Wrapper downcasting

The deterministic `get_wrap` panic claim is obsolete.
The current erased wrapper trait exposes the concrete value through `as_any`, and lookup downcasts that value rather than the outer box.
Existing composition tests exercise wrapper lookup and ordering.

### Windows thread enumeration

`resume_threads` still uses a system-wide thread snapshot as a fallback when no wrapper capability can resume the child directly.
It rejects a zero process ID before taking the snapshot.
A live suspended process handle narrows the claimed PID-reuse scenario, and the report does not establish a current correctness defect.
The snapshot remains a compatibility and performance compromise rather than an unsafe-code finding.

### Job-object handle retention

The report overstates the scope of the leak.
Standard-library job objects and Tokio jobs without finalized `KillOnDrop` close their handles normally.
A finalized Tokio `KillOnDrop` layer deliberately retains the job handle when removed through generic unsafe traversal because closing the final kill-on-close handle may terminate the returned child.
The hidden resource ownership is an API-design candidate documented in [api-hardening.md](api-hardening.md).

### Negative process-group `waitpid`

The reported negative-PGID reaping path no longer exists.
Current process-group wrappers wait for the direct child and stop using a numeric group ID after that child is reaped.

### Missing safety rationales

The blanket claim that every unsafe site lacked documentation is false on current `main`.
Several Unix PTY and child-access boundaries already have detailed comments and public contracts.
Windows job-object code, standard dual-pipe reads, several unsafe implementations, and test call sites still need local proofs.
That non-runtime work is specified in [unsafe-hardening.md](unsafe-hardening.md).

## Classification A: make APIs harder to misuse

### Validate process-group IDs at construction

`ProcessGroup::attach_to(u32)` accepts zero and out-of-range values and reports the error only during spawn.
A checked constructor or validated process-group ID type can reject these states before the wrapper enters a command.
See [api-hardening.md](api-hardening.md).

### Make job-object removal ownership explicit

Generic unsafe unwrapping can hide intentional job-handle retention when kill-on-close is armed.
A fallible detach operation or an `into_parts` result carrying an owning job guard can expose the resource and lifecycle outcome.
See [api-hardening.md](api-hardening.md).

### Narrow post-fork extension points

The public native command abstraction carries an unsafe callback registration method even though backend implementation is sealed.
Complete safety documentation belongs in the current hardening work.
A separate API review can decide whether sealing the native-command trait or introducing a prepared child-setup abstraction removes unsupported extension points without pretending arbitrary closures are safe.

## Classification B: other code changes

### Repair Windows job completion

`wait_on_job` currently treats any dequeued completion packet as terminal, ignores the job message identifier, and conflates finite-timeout failures with a timeout.
Job ports emit process-created and process-exited notifications in addition to `JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO`.
A polling `try_wait` can consume the terminal packet without caching completion, after which a later wait can block.

The repair needs a packet-filtering loop, precise timeout/error handling, a deadline for finite waits, and cached terminal state shared by `try_wait` and `wait`.
It also needs a native Windows regression test where the direct child exits while a descendant remains active.
This is a runtime correctness fix and does not belong in the comment/Miri pull request.

### Encode private raw-pointer preconditions

The Windows environment-block parser accepts a raw pointer through a safe private function while relying on readable double-NUL-terminated storage.
The SCM_RIGHTS decoder accepts an `msghdr` while relying on its control pointer remaining tied to a live receive buffer.
Their current production callers establish the invariants, but the function signatures do not express them.

A separate change should introduce owning wrappers or unsafe function contracts and then extract pure parsing as described in [ffi-boundary-extraction.md](ffi-boundary-extraction.md).

### Resolve the PTY post-fork `ioctl` boundary

The Unix PTY callback must acquire a controlling terminal after creating a session.
Its comment currently groups `ioctl(TIOCSCTTY)` with functions guaranteed async-signal-safe by POSIX, but `ioctl` is not on POSIX's mandatory list.
The OpenBSD PTY helper has a related target-specific `PTMGET` operation after fork.

The immediate documentation change must record the supported-target libc assumption rather than claim a portable POSIX guarantee.
Architectural alternatives are a target-capability `posix_spawn` provider, a minimal audited post-fork component, or native target evidence.
Those approaches are drafted separately and require code beyond the hardening pull request.

### Separate FFI ingestion from state logic

Windows completion packets, environment blocks, and ancillary descriptor buffers currently combine unsafe ingestion with parsing or transitions.
Extracting typed values and pure state machines is an internal code-design change described in [ffi-boundary-extraction.md](ffi-boundary-extraction.md).

## Non-code changes in the current pull request

The following findings are addressed without changing runtime behavior:

- complete and correct local safety rationales;
- aligned public `# Safety` documentation;
- unsafe-operation and documentation lints;
- native, MSRV, and portable-target Clippy coverage;
- selective process-free Miri coverage;
- explicit documentation of what each check cannot prove.
