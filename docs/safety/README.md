# Process safety design drafts

These documents split the process-safety work into proposals with different evidence and implementation costs.
They describe the command, spawn-provider, Unix dispatcher, PTY, and Windows job-object architecture on `main` after the command/provider refactor.

Only [unsafe hardening](unsafe-hardening.md) and [selective Miri](miri.md) are approved for implementation in the current work.
The other documents are drafts for separate efforts and do not commit the current pull request to runtime or public API changes.

| Draft | Purpose |
| --- | --- |
| [Unsafe hardening](unsafe-hardening.md) | Complete safety rationales and enforce useful unsafe-code lints across supported configurations. |
| [Audit disposition](audit-disposition.md) | Classify historical, API-hardening, runtime, and documentation findings against current `main`. |
| [Selective Miri](miri.md) | Interpret the process-free command, provider, registry, and child-traversal paths. |
| [Harder-to-misuse APIs](api-hardening.md) | Validate identifiers earlier and make wrapper removal and post-fork escape hatches more explicit. |
| [FFI boundary extraction](ffi-boundary-extraction.md) | Move parsing and state transitions behind small unsafe OS adapters. |
| [Post-fork allocation guard](post-fork-allocation-guard.md) | Detect allocation and deallocation after `fork` with a native test allocator. |
| [Post-fork tripwires](post-fork-tripwires.md) | Add Linux-only tracing-lock and syscall regression checks. |
| [Minimal post-fork core](minimal-post-fork-core.md) | Isolate child setup into code that can compile without `std` or `alloc`. |
| [`posix_spawn` provider](posix-spawn-provider.md) | Avoid custom post-fork callbacks for setup expressible through spawn attributes. |
| [Native platform CI](native-platform-ci.md) | Execute target-specific PTY and FFI paths on their actual kernels. |

A separate spawn-broker process is intentionally not drafted.
