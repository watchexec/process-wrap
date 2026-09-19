# FFI boundary extraction

Status: draft for a separate effort.

## Goal

Reduce the amount of logic that executes while raw pointers, operating-system buffers, or untyped completion values are live.
Each boundary should follow this shape:

```text
unsafe OS ingestion -> bounded owned or borrowed Rust value -> pure parser or state machine
```

This improves reviewability and allows Miri and property tests to cover logic that currently sits inside an FFI-bearing function.
It does not make the operating-system call itself interpretable by Miri.

## Windows environment blocks

### Current boundary

The environment parser accepts `PWSTR` and scans until a double NUL.
The function is safe despite requiring the pointer to identify a readable, initialized, double-NUL-terminated block.
Production establishes that invariant through `GetEnvironmentStringsW`, while tests manufacture compatible storage.

### Proposed boundary

Make `EnvironmentBlock` the only owner of a native block returned by `GetEnvironmentStringsW`.
Give it a method that performs the one unsafe bounded scan while the guard remains alive and returns an owned `Vec<u16>` or a lifetime-bound view.
Move entry splitting, pseudo-variable handling, key validation, and ordering into a safe `parse_environment_units(&[u16])` function.
Tests should exercise the slice parser without constructing raw pointers.

The scan still needs a documented external termination invariant because the Win32 API does not return a length.
Only that scan should rely on the invariant.

## SCM_RIGHTS control messages

### Current boundary

`ReceivedRights::decode` accepts an arbitrary `msghdr` by shared reference and then dereferences `msg_control` under the assumption that it still points into the live receive buffer.
Bounds and protocol checks occur after entering the function.

### Proposed boundary

Create a receive-buffer type that owns the control storage and `msghdr` for the complete `recvmsg` operation.
Expose a safe decode method only while the header is tied to that storage.
Copy the bounded header and descriptor payload into a small Rust representation before applying protocol checks.

The pure decoder should validate:

- one first control message with the expected level and type;
- exact header and payload lengths;
- no truncation;
- at most two complete descriptors;
- nonnegative, distinct descriptors;
- expected response error semantics.

Ownership conversion to `OwnedFd` remains a small unsafe step after the protocol has established that the kernel installed distinct descriptors.

## Windows job completion packets

### Current boundary

`GetQueuedCompletionStatus` returns an untyped message identifier, key, overlapped pointer, and success state.
Current job waiting treats packet arrival as terminal completion even though job ports report several message kinds.

### Proposed boundary

Convert one native dequeue into a typed value:

```rust
#[derive(Debug)]
enum JobPacket {
    ActiveProcessZero,
    NewProcess,
    ExitProcess,
    Other(u32),
    Timeout,
}
```

Keep handle validity and output-pointer initialization in the FFI adapter.
Move packet filtering, deadline handling, and cached terminal state into a pure state machine.
Miri and ordinary unit tests can then cover sequences such as `NewProcess`, `ExitProcess`, and `ActiveProcessZero` without a Windows kernel object.

This extraction enables but does not itself implement the runtime completion-port repair.

## Unix child setup plans

The parent should prepare a fixed-size child setup value containing only validated process-group, session, and signal-mask policy.
Pure validation should reject incompatible or unrepresentable states before spawn.
The child executor should read that representation without allocation or parent-side locks.

The current centralized dispatcher already approximates this separation with atomics.
A follow-up should evaluate whether an explicit `ChildSetupPlan` can narrow proofs without losing reusable native-command behavior.

## Validation

- Run pure parsers and state machines under Miri.
- Add property tests for malformed environment entries, ancillary lengths, duplicate descriptors, and completion-packet orderings.
- Keep native integration tests for API ownership, kernel-filled layouts, descriptor installation, and handle behavior.
- Confirm that no production behavior changes until the extracted implementation is compared against current tests.

## Acceptance criteria

- Raw pointers do not enter general parsing or state-transition code.
- Safe functions accept inputs whose invariants are expressible in their Rust types.
- Each unsafe adapter has one local contract covering the complete external invariant.
- Pure logic executes under Miri without FFI or target-specific substitutes.
