# Unified command API and PTY provider

Process-wrap 10.0 established a reliable ordered wrapper registry and child capability model.
The PTY prototypes proved the Unix and ConPTY transports but introduced `PtyCommand` as a second command-building and spawning API.
Process-wrap 11 will instead make command configuration exact and transport-independent, with PTY selected by the same `.wrap(Pty).spawn()` flow as every other concern.

## Shared command family

Introduce one generic command implementation selected by sealed blocking and Tokio frontend markers.
Re-export backend-selected `Command` aliases from the existing `std` and `tokio` modules.
Retain `CommandWrap` as an alias while preferring `Command` in new documentation.
Keep backend-specific `spawn` implementations and child traits because blocking and Tokio child operations have different contracts.
Keep both Cargo frontend features additive and independently usable.

Move program, argument, environment, cwd, wrapper registry, and common construction logic into the shared command implementation.
Keep `with_new`, with its closure receiving the process-wrap command instead of a native command.
Expose native-shaped tracked configuration methods so inferred existing closure bodies remain valid.
Preserve compatibility command accessors as process-wrap facade views rather than native escape hatches.
Add explicitly named native mutation and conversion methods.

## Exact and native-only state

Represent ordinary command configuration as cloneable tracked intent.
Preserve ordered regular and raw Windows argument operations without Unicode normalization.
Preserve inherited versus cleared environment state, environment mutations, and cwd exactly.
Materialize a fresh native command from tracked intent for each spawn attempt.

Retain conversion from native std and Tokio commands as a native-only compatibility state.
Transition tracked state to native-only when callers explicitly request native mutation or configure state that cannot be reconstructed, such as arbitrary `Stdio` handles or `pre_exec` callbacks.
Allow native-only commands to use exact native spawning and explicit custom spawners.
Reject native-only state from portable providers rather than reconstructing, ignoring, or guessing at it.

## Attempt and wrapper lifecycle

Create a per-spawn attempt facade from tracked command state.
Change `pre_spawn` to mutate that facade while retaining read-only access to live peer wrappers.
Change `post_spawn` to receive the attempt and the frontend child capability trait rather than requiring a native child.
Run `post_spawn` for native, custom, and provider children.
Retain ordered `wrap_child` application and typed duplicate-wrapper extension.
Preserve wrapper slots and command state across success, error, and panic paths.

Move built-in process group, process session, signal-mask, creation-flag, and kill-on-drop setup onto tracked attempt policy where possible.
Use object-safe child capabilities for process handles and provider-owned suspension rather than concrete custom-child downcasts.
Let custom wrappers compose based on representable command state and available child capabilities instead of concrete type allowlists.

## Spawn providers

Add an object-safe frontend-specific provider capability exposed by a command wrapper.
Select the native provider when no alternate provider is registered.
Reject multiple alternate providers before hooks or operating-system allocation.
Run provider availability checks before other provider validation so unsupported-platform errors retain precedence.
Reject incompatible immutable configuration before hooks.
Run ordered pre-spawn hooks, then validate the resulting attempt before provider allocation.
Have providers return boxed frontend children and armed cleanup transactions.
Run ordered post-spawn and child-wrapping hooks before committing the provider transaction.
Ensure provider cleanup runs after every later error or panic without replacing the original failure.

Treat `spawn_with` and `spawn_with_child` as explicit caller-selected transports.
Reject either method when a wrapper provider is registered instead of bypassing it.
Run capability-level post-spawn and ordinary child wrapping for both methods.

## Tokio PTY wrapper

Replace `PtyCommand`, its duplicate command intent and wrapper registry, `PtyMarker`, and tuple-returning spawn with a Tokio-only `Pty` provider wrapper.
Store terminal size on `Pty` and merge duplicate registration through typed extension.
Keep ordinary `spawn` returning the boxed Tokio child contract.
Install a private child layer which owns the PTY controller beneath arbitrary outer wrappers.
Add one-shot controller extraction by traversing the child chain without unwrapping it.
Keep PTY input and output on the controller and keep native Tokio pipe accessors absent.

## Unix PTY transport

Reuse the existing PTY allocation and Tokio I/O implementation for Android, DragonFly BSD, FreeBSD, illumos, Linux, macOS, NetBSD, OpenBSD, and Solaris.
Preserve close-on-exec, nonblocking master I/O, slave duplication, controlling-terminal setup, foreground process groups, Linux EIO normalization, and cleanup behavior.
Apply slave stdio and terminal callbacks only to the fresh attempt command.
Close parent slave descriptors before post-spawn and child-wrapping hooks.
Preserve shared master ownership by input and output, weak resize ownership, merged terminal output, and independent child-wait and output-EOF lifecycles.

Integrate real `Pty` registration with `ProcessGroup`, `ProcessSession`, `ResetSigmask`, and `KillOnDrop`.
Preserve group-aware supervision for leader/session modes.
Reject attached groups and simultaneous explicit group/session registration.
Return unsupported-platform errors before terminal-size validation or hook effects.

## Exact Windows command model

Reuse the existing Windows argument, environment, program-resolution, and cwd modules against shared tracked command intent.
Preserve interleaved regular and raw argument semantics, CRT quoting, WTF-16 data, and stable validation errors.
Preserve native environment inheritance, explicit Unicode blocks, case-insensitive key replacement, deterministic ordering, and drive pseudo-variables.
Preserve deterministic executable resolution and direct batch-script rejection.
Carry creation flags, explicit versus temporary suspension, JobObject, and KillOnDrop through tracked policy and child capabilities.

## ConPTY transport

Reuse the existing dynamic ConPTY API resolution, startup attributes, named pipes, manual `CreateProcessW`, custom child, controller, and cleanup modules.
Integrate them as the Tokio `Pty` provider rather than a separate spawn method.
Preserve unsupported-runtime precedence and exact command line, environment, cwd, flag, and handle-inheritance semantics.
Retain process and primary-thread handles through JobObject assignment and provider-owned suspension finalization.
Keep the cleanup guard armed through all wrapper hooks and disarm it only when the provider transaction commits.
Preserve cancellation-safe repeated waits, post-exit kill behavior, resize, merged I/O, direct-child and job-object kill-on-drop behavior, and off-reactor pseudoconsole closure.

## Public documentation and migration

Prefer the backend module `Command` aliases while retaining `CommandWrap` compatibility.
Show inferred `with_new` closures continuing to use native-shaped configuration calls.
Explain why backend typing remains at spawn and child boundaries even though command configuration is shared.
Use only `.wrap(Pty).spawn()` and one-shot controller extraction in PTY examples.
Preserve the crate’s adaptability motivation and complete supported Unix platform list.
Preserve the terminal stream, ownership, VEOF, draining, macOS lifecycle, and process-supervision rationale.
Explain that `pty` remains non-default because it selects Tokio and PTY dependencies.
Remove the rejected public PTY builder, tuple spawn, marker names, fallback-to-pipes wording, and CI-runner prose.
Document native-only escape behavior and the migration from the former PTY prototype.
