[![Crate release version](https://flat.badgen.net/crates/v/process-wrap)](https://crates.io/crates/process-wrap)
[![Crate license: Apache 2.0 or MIT](https://flat.badgen.net/badge/license/Apache%202.0%20or%20MIT)][copyright]
[![CI status](https://github.com/watchexec/process-wrap/actions/workflows/test.yml/badge.svg)](https://github.com/watchexec/process-wrap/actions/workflows/test.yml)

# process-wrap

- **[API documentation][docs]**.
- [Dual-licensed][copyright] with Apache 2.0 and MIT.
- Successor to [command-group](https://github.com/watchexec/command-group).
- Minimum Supported Rust Version: 1.87.0.
  - Only the latest stable rustc version is supported.
  - We try to conservatively bump the MSRV,
  - but MSRV increases will not incur major version bumps.

[copyright]: ./COPYRIGHT
[docs]: https://docs.rs/process-wrap

Unlike command-group, process-wrap doesn't implement a single cross-platform API.
Instead, it provides composable wrappers which implement a single concern each.
It is left to the developer to use the appropriate wrapper(s) for their use-case and platform.

As the successor to (and containing a lot of the code of) command-group, versioning starts at 6.0.0.
You can think of it as a breaking change to command-group, though the paradigm is quite different.
The full test suite from command-group was retained: process-wrap has parity on functionality as a starting point.

## Quick start

```toml
[dependencies]
process-wrap = { version = "10.0.0", features = ["tokio1"] }
```

By default, the crate does nothing, you need to enable either the std or Tokio "frontend". A default
set of wrappers are enabled; you may choose to only compile those you need, see [the features list].

Both frontends use the same process-wrap `Command` configuration API. The frontend selected by the
module controls spawning and the child contract: std child operations block, while Tokio child
operations are asynchronous. `CommandWrap` remains an alias for compatibility. Enabling both
frontends exposes both `process_wrap::std::Command` and `process_wrap::tokio::Command` without one
taking precedence.

```rust
use process_wrap::tokio::*;

let mut child = Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessGroup::leader())
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or on Windows

```rust
use process_wrap::tokio::*;

let mut child = Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(JobObject)
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or with sessions

```rust
use process_wrap::tokio::*;

let mut child = Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessSession)
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or with multiple wrappers

```rust
use process_wrap::tokio::*;

let mut child = Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessSession)
  .wrap(KillOnDrop)
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or in a pseudo-terminal

The non-default `pty` feature enables Tokio PTY transport on Linux, Android, macOS, FreeBSD,
NetBSD 10 and newer, OpenBSD, DragonFly BSD, illumos, and Solaris. It implies `tokio1`, selecting
the Tokio frontend and its terminal dependencies explicitly.

```toml
[dependencies]
process-wrap = { version = "11.0.0", features = ["pty"] }
```

```rust
use process_wrap::tokio::*;
use tokio::io::AsyncReadExt;

let mut command = Command::new("ls");
command.wrap(ProcessSession).wrap(Pty::default());
let mut child = command.spawn()?;
let controller = child
  .take_pty_controller()
  .expect("a successful PTY spawn installs one controller");
let (input, mut output, _resize) = controller.into_parts();
drop(input);

let drain = tokio::spawn(async move {
  let mut bytes = Vec::new();
  output.read_to_end(&mut bytes).await?;
  Ok::<_, std::io::Error>(bytes)
});
let status = child.wait().await?;
let terminal_bytes = drain.await??;
dbg!(status, terminal_bytes);
```

A PTY has one ordered terminal stream, so standard output and standard error are merged.
`PtyInput` and `PtyOutput` are strong owners of one bidirectional master descriptor, so dropping
either one alone does not half-close the terminal. The terminal hangs up after both are gone;
`PtyResize` is weak and cannot keep it alive. Send the terminal's VEOF character when that is the
desired terminal policy instead of expecting a separate input half-close or clonable force-close
handle.

Child waiting and PTY draining are independent. On most supported Unix systems, descendants can
retain the slave after the direct child exits. On macOS, drain output concurrently with waiting: the
kernel drains queued output as the session leader exits, then revokes the controlling terminal from
its descendants. The transport passes terminal bytes through without owning parent-terminal raw
mode, relays, key handling, VT parsing, scrollback, or pager policy.

A bare PTY creates the required session. `ProcessGroup::leader()` and `ProcessSession` each preserve
group-wide signalling while the direct child is live; waiting still follows that direct child.
`ProcessGroup::attach_to(...)` and explicitly registering both wrappers return `InvalidInput`.
`ResetSigmask` composes normally. `KillOnDrop` remains Tokio's direct-child behavior—it does not
promise to kill an entire group or session. Spawning returns the ordinary boxed Tokio child, and
`take_pty_controller()` traverses any outer child wrappers and yields the controller once.

### or with std

```toml
[dependencies]
process-wrap = { version = "10.0.0", features = ["std"] }
```

```rust
use process_wrap::std::*;

let mut child = Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessGroup::leader())
  .spawn()?;
let status = child.wait()?;
dbg!(status);
```

### Native command compatibility

Commands built through `Command::new`, `Command::with_new`, and the process-wrap configuration
methods retain exact portable command intent and create a fresh native command for every spawn
attempt. The `with_new` closure now receives process-wrap's `Command`; inferred calls such as
`command.arg(...)` continue unchanged.

`Command::from(native_command)` preserves an existing std or Tokio command as native-only state.
`native_mut()` and non-reconstructable configuration such as arbitrary `Stdio` do the same.
Native-only commands retain exact ordinary spawning and `spawn_with*` behavior, but alternate
portable transports cannot recover raw argument tags, environment-clear history, `pre_exec`
callbacks, or arbitrary native handles and will reject that state. Use `into_native()` when the rest
of the lifecycle belongs to the native API.

The facade keeps native stable configuration methods where their behavior can be preserved, including
Unix identity setup, the standard frontend's process-group setter, Windows creation flags, and Tokio
kill-on-drop. These platform-specific operations make the command native-only. The standard library's
supplementary-groups setter remains unstable and is not mirrored by the facade. Tokio's process-group
setter is also omitted at the declared Tokio floor because it cannot exactly replace process-group
state already stored in a native-only Tokio command. Use the `ProcessGroup` wrapper for tracked Tokio
commands, or configure a `std::process::Command` before converting it into Tokio and then process-wrap.
An immutable Tokio `as_std()` requires the explicit `command.native_mut().as_std()` transition. Tokio
1.38.2 does not expose mutable access to its inner standard command, so use the same conversion path
when that escape is needed.

## Wrappers

### Job object

- Platforms: Windows
- Like command-group.
- Feature: `job-object` (default)

```rust
Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(JobObject)
  .spawn()?;
```

`CreationFlags` and `JobObject` may be registered in either order. `JobObject` preserves every
requested flag, temporarily adds `CREATE_SUSPENDED` while assigning the process, and resumes it
after assignment unless the caller explicitly requested `CREATE_SUSPENDED`.

### Process group

- Platforms: POSIX (Linux, Mac, BSDs...)
- Like command-group >=5.0.0.
- Feature: `process-group` (default)

```rust
Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessGroup::leader())
  .spawn()?;
```

Or join a different group instead:

```rust
Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessGroup::attach_to(pgid))
  .spawn()?;
```

For Windows process groups, use `CreationFlags::NEW_PROCESS_GROUP` and/or `JobObject`.

### Process session

- Platforms: POSIX (Linux, Mac, BSDs...)
- Like command-group <5.0.0.
- Feature: `process-session` (default)

This combines creating a new session and a new group, and setting this process as leader.
To join the session from another process, use `ProcessGroup::attach_to()` instead.

```rust
Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ProcessSession)
  .spawn()?;
```

### Reset signal mask

- Platforms: POSIX (Linux, Mac, BSDs...)
- Feature: `reset-sigmask`

This resets the [signal mask] of the process instead of inheriting it from the parent.

[signal mask]: https://www.man7.org/linux/man-pages/man2/sigprocmask.2.html

```rust
Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(ResetSigmask)
  .spawn()?;
```

### Creation flags

- Platforms: Windows
- Like command-group.
- Feature: `creation-flags` (default)

This wrapper records Windows creation flags as portable per-attempt policy. Calling the native-shaped
`Command::creation_flags` method instead makes the command native-only because wrappers and alternate
transports cannot query or reconstruct those flags.

```rust
use windows::Win32::System::Threading::*;
Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(CreationFlags(CREATE_NO_WINDOW | CREATE_DETACHED))
  .wrap(JobObject)
  .spawn()?;
```

`CreationFlags` and `JobObject` may be registered in either order. `JobObject` preserves every
requested flag, temporarily adds `CREATE_SUSPENDED` while assigning the process, and resumes it
after assignment unless the caller explicitly requested `CREATE_SUSPENDED`.

### Kill on drop

- Platforms: all (Tokio-only)
- Like command-group.
- Feature: `kill-on-drop` (default)

This wrapper records kill-on-drop as portable per-attempt policy so `JobObject` and alternate spawn
providers can preserve it. Calling the native-shaped `Command::kill_on_drop` method instead makes the
command native-only because the setting cannot be queried afterward.

```rust
let child = Command::with_new("watch", |command| { command.arg("ls"); })
  .wrap(KillOnDrop)
  .wrap(ProcessGroup::leader())
  .spawn()?;
drop(child);
```

### Your own

Implementing a wrapper is done via a set of traits.
Command configuration is shared, but std and Tokio wrappers remain separate because their spawn and
child APIs differ. Re-use shared policy code when implementing both frontends.

At minimum, you must implement `CommandWrapper` (from `process_wrap::std` and/or `process_wrap::tokio`).
These provide the same functionality, but differ in the exact types specified.
Here's the most basic impl (shown for Tokio):

```rust
#[derive(Debug)]
pub struct YourWrapper;
impl CommandWrapper for YourWrapper {}
```

That's right, all member methods are optional.
The trait provides extension or hook points into the lifecycle of a `Command`:

- **`fn extend(&mut self, other: Self)`** is called if `.wrap(YourWrapper)` is done twice.
  Only one wrapper of a given type can exist, so this gives the stored instance an opportunity to
  incorporate all or part of the second, concretely typed wrapper. By default, this does nothing
  (that is, only the first registered wrapper instance of a type applies).

- **`fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, command: &Command) -> io::Result<()>`**
  is called before spawning. It can record portable configuration on this attempt and inspect peer
  wrappers through `command`. For tracked commands those mutations apply to one attempt; native-only
  commands retain native mutations. Calling `attempt.native_mut()` or its `stdin`/`stdout`/`stderr`
  methods makes a tracked attempt incompatible with a portable provider. On Unix, recurring native
  escapes from a reusable native-only command can retain inactive child-setup callbacks because the
  native API does not expose callback insertion or command ownership; prefer portable attempt methods
  for recurring configuration.

- **`fn post_spawn(&mut self, attempt: &mut SpawnAttempt, child: &mut dyn ChildWrapper, command: &Command) -> io::Result<()>`**
  is called after any transport has created its child. The child may be a terminal custom/provider
  child with no native child value. Changing command settings on `attempt` at this point cannot
  configure the already-created child.

- **`fn wrap_child(&mut self, child: Box<dyn ChildWrapper>, command: &Command) -> io::Result<Box<dyn ChildWrapper>>`**
  is called after all `post_spawn()` hooks. If your wrapper needs to override child methods, create
  your own `ChildWrapper` layer and return it here. Child wraps run in registration order, so
  `.wrap(Foo).wrap(Bar)` produces an outer `Bar(Foo(child))`.

- **`fn spawn_provider(&self) -> Option<&dyn SpawnProvider>`** exposes an alternate transport owned by
  this wrapper. A provider exposed during selection must remain available throughout the lifecycle;
  only one registered wrapper may expose one.

Pre-spawn, post-spawn, and child-wrapping hooks all run in registration order and stop at the first
error or panic. The active wrapper remains registered but is temporarily unavailable through
`get_wrap`; peer wrappers remain visible.

### Spawn providers

Spawn providers let a wrapper replace only process creation while retaining the complete wrapper
lifecycle. This is what makes custom transports such as PTYs composable with process-wrap wrappers.
Callbacks run in this order:

1. `check_available`
2. native-only base rejection
3. `validate_command`
4. every `pre_spawn` hook
5. native-only attempt rejection
6. `validate_attempt`
7. provider `spawn`
8. every `post_spawn` hook
9. every child wrapper
10. transaction `commit`

Validation must reject unsupported portable policy before allocating operating-system resources.
`spawn` returns a child satisfying the frontend's complete `ChildWrapper` contract together with a
fresh, armed `SpawnTransaction`. The transaction owns cleanup independently of the child chain. A
later hook, wrapper, or commit error/panic causes best-effort rollback while preserving the original
failure. Until `spawn` returns the product, cleanup remains the provider's responsibility.

A command may register only one provider; conflicts are rejected before any provider callback or
operating-system allocation. Both providers and wrapper state are reused across repeated spawns.
`spawn_with` and `spawn_with_child` reject a registered provider instead of silently bypassing it.
On Unix, when wrappers request built-in child setup, a successful explicit spawner must create its
returned child from the native command before replacing that command. Whenever the spawner replaces
it, including before returning an error or unwinding, the displaced command must be dropped before
control leaves the spawner. A replacement is discarded with a tracked attempt or retained by a
native-only base. Process-wrap installs child setup before invoking the spawner and cannot apply it to
a replacement which the closure creates and immediately spawns.

Refer to [the API documentation][docs] for the policy getters, platform child capabilities, and the
specifics of child wrapper traits.

## Features
[the features list]: #features

### Frontends

- `std`: enables the std-based API.
- `tokio1`: enables the Tokio-based API.

Both can exist at the same time, but generally you should use one or the other.

### Wrappers

- `creation-flags`: **default**, enables the [creation flags](#creation-flags) wrapper.
- `job-object`: **default**, enables the [job object](#job-object) wrapper.
- `kill-on-drop`: **default**, enables the [kill on drop](#kill-on-drop) wrapper.
- `process-group`: **default**, enables the [process group](#process-group) wrapper.
- `process-session`: **default**, enables the [process session](#process-session) wrapper.
- `pty`: enables the Tokio [pseudo-terminal transport](#or-in-a-pseudo-terminal) and implies
  `tokio1`.
- `reset-sigmask`: enables the [reset signal mask](#reset-signal-mask) wrapper.

### Diagnostics

- `tracing`: **default**, enables internal lifecycle diagnostics through the `tracing` crate.
