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
Unix identity/process setup, Windows creation flags, and Tokio kill-on-drop. These platform-specific
operations make the command native-only. The standard library's supplementary-groups setter remains
unstable and is not mirrored by the facade. An immutable Tokio `as_std()` cannot borrow a native
command which has not yet been materialized; use `command.native_mut().as_std()` when that view is
required. Tokio 1.38.2 does not expose mutable access to its inner standard command, so configure a
`std::process::Command` first and convert it into Tokio and then process-wrap when that escape is
needed.

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

This is a shim to allow setting Windows process creation flags with this API, as otherwise they'd be overwritten.

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

This is a shim to allow wrappers to handle the kill-on-drop flag, as it can't be read from Command.

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

- **`fn pre_spawn(&mut self, command: &mut tokio::process::Command, core: &Command)`** is called
  before the command is spawned, and gives mutable access to that attempt's native command. It also
  gives mutable access to the wrapper instance, so state can be stored if needed. The `core`
  reference gives access to data from other wrappers; for example, that's how `CreationFlags` on
  Windows works along with `JobObject`. Noop by default.

- **`fn post_spawn(&mut self, command: &mut tokio::process::Command, child: &mut tokio::process::Child, core: &Command)`**
  is called after spawn, and should be used for any necessary cleanups. It is offered for completeness
  but is expected to be less used than `wrap_child()`. Noop by default.

- **`fn wrap_child(&mut self, child: Box<dyn ChildWrapper>, core: &Command)`** is
  called after all `post_spawn()`s have run. If your wrapper needs to override the methods on Child,
  then it should create an instance of its own type implementing `ChildWrapper` and return it
  here. Child wraps are _in order_: you may end up with a `Foo(Bar(Child))` or a `Bar(Foo(Child))`
  depending on if `.wrap(Foo).wrap(Bar)` or `.wrap(Bar).wrap(Foo)` was called. If your functionality
  is order-dependent, make sure to specify so in your documentation! Default is noop: no wrapping is
  performed and the input `child` is returned as-is.

Refer to [the API documentation][docs] for more detail and the specifics of child wrapper traits.

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
- `reset-sigmask`: enables the [reset signal mask](#reset-signal-mask) wrapper.
