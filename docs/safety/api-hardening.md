# Harder-to-misuse API candidates

Status: draft for a separate effort.

## Goal

Turn invalid or resource-sensitive operations into states and methods that make their obligations visible before spawning or unwrapping a child.
This document describes public API changes and is not part of the unsafe-comment and Miri pull request.

## Validated process-group identifiers

### Current behavior

`ProcessGroup::attach_to` accepts any `u32`.
Zero and values above the platform `pid_t` range are rejected later by `pre_spawn`, so construction succeeds and an otherwise prepared spawn attempt fails.

### Options

1. Add `ProcessGroup::try_attach_to(u32) -> io::Result<Self>` and retain the current constructor for compatibility.
2. Introduce a validated `ProcessGroupId` newtype and require it when attaching.
3. Change `attach_to` itself to return a result in a release that permits the API break.

The additive path should introduce `try_attach_to` first and direct new documentation to it.
A future breaking release can make the validated form primary.
The validated representation must reject zero and values that do not fit the target `pid_t` representation.

### Acceptance criteria

- Invalid identifiers fail at construction rather than during spawn when callers choose the checked API.
- The child wrapper stores the configured existing group rather than deriving it from the direct child's PID.
- Documentation distinguishes new-group leadership from attachment to an existing group.

## Job-object unwrapping and handle ownership

### Current behavior

Removing a finalized Tokio job-object layer configured with `KillOnDrop` cannot simply close the final job handle because that may terminate the returned child.
The generic unsafe child-unwrapping path therefore closes the completion port and deliberately retains the job handle.
The resource cost is hidden inside an operation whose visible result is only the inner child.

### Candidate APIs

A Windows-specific API should make one of two ownership outcomes explicit:

- `detach_job` clears `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, closes both handles, and returns the inner child only if every Win32 operation succeeds;
- `into_parts` returns the inner child together with an owning `JobObjectGuard`, transferring responsibility for the job and completion-port handles without leaking them.

The guard form preserves job semantics and makes resource lifetime visible.
The detach form is convenient when the caller wants the child to outlive the wrapper and no longer needs job-wide supervision.

The generic traversal API cannot silently provide either guarantee because wrapper chains may contain arbitrary layers and the existing trait's `into_inner` operation is infallible.
A design may therefore need a fallible wrapper capability or a direct method on `JobObjectChild` before type erasure.

### Acceptance criteria

- No successful explicit unwrap requires an unreachable retained kernel handle.
- Failure to clear kill-on-close does not accidentally terminate a child that is being returned.
- The API documents whether descendants remain supervised and what dropping the returned guard does.
- Generic unsafe traversal continues to state that bypassing wrapper cleanup is the caller's responsibility.

## Post-fork callback escape hatches

### Current behavior

`Command` and `SpawnAttempt` expose unsafe `pre_exec` methods, and the public `NativeCommand` abstraction carries the same operation.
The operation is inherently unsafe because arbitrary closures cannot be checked for post-`fork` validity.

### Near-term hardening

The current hardening effort aligns every `# Safety` section and explicitly names allocation, lock acquisition, environment access, formatting, and non-audited library calls.
This documentation work does not make the callback safe.

### Structural options

- Seal `NativeCommand` if external implementations have no supported backend use, preventing unaudited command adapters from extending the internal unsafe boundary.
- Introduce an unsafe child-setup trait with a prepared parent-side state and a narrowly defined child-side method when reusable setup components are needed.
- Keep raw callback registration as the escape hatch rather than wrapping it in a nominal safe type that cannot enforce its contract.

A marker or newtype around an arbitrary closure does not improve safety by itself.
Any replacement must encode a real invariant or should not be introduced.

## Non-goals

This proposal does not make operating-system setup infallible.
It does not make arbitrary post-fork callbacks safe.
It does not weaken the existing unsafe warning on generic child traversal.
It does not bundle Windows completion-port correctness changes, which are runtime repairs rather than API hardening.
