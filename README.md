[![Crate release version](https://flat.badgen.net/crates/v/process-wrap)](https://crates.io/crates/process-wrap)
[![Crate license: Apache 2.0 or MIT](https://flat.badgen.net/badge/license/Apache%202.0%20or%20MIT)][copyright]
[![CI status](https://github.com/watchexec/process-wrap/actions/workflows/test.yml/badge.svg)](https://github.com/watchexec/process-wrap/actions/workflows/test.yml)

# process-wrap

- **[API documentation][docs]**.
- [Dual-licensed][copyright] with Apache 2.0 and MIT.
- Successor to [command-group](https://github.com/watchexec/command-group).
- Minimum Supported Rust Version: 1.75.0.
  - Only the last two stable versions are supported.
  - MSRV increases within that range at publish time will not incur major version bumps.

[copyright]: ./COPYRIGHT
[docs]: https://docs.rs/process-wrap

Unlike command-group, process-wrap doesn't implement a single cross-platform API.
Instead, it provides composable wrappers which implement a single concern each.
It is left to the developer to use the appropriate wrapper(s) for their use-case and platform.

As the successor to (and containing a lot of the code of) command-group, versioning starts at 6.0.0.
You can think of it as a breaking change to command-group, though the paradigm is quite different.

## Quick start

```toml
[dependencies]
process-wrap = "6.0.0"
```

### with std

```rust
use std::process::Command;
use process_wrap::std::*;

let mut child = StdCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(ProcessGroup::leader())
  .spawn()?;
let status = child.wait()?;
dbg!(status);
```

### or with Tokio

```rust
use tokio::process::Command;
use process_wrap::tokio::*;

let mut child = TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(ProcessGroup::leader())
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or on Windows

```rust
use tokio::process::Command;
use process_wrap::tokio::*;

let mut child = TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(JobObject::new())
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or with sessions

```rust
use tokio::process::Command;
use process_wrap::tokio::*;

let mut child = TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(ProcessSession::leader())
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

### or with sessions and groups

```rust
use tokio::process::Command;
use process_wrap::tokio::TokioCommandWrap;

let mut child = TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(ProcessSession::leader())
  .wrap(ProcessGroup::leader())
  .spawn()?;
let status = child.wait().await?;
dbg!(status);
```

## Wrappers

### Job object

- Platforms: Windows
- Like command-group.
- Feature: `job-object` (default)

### Process group

- Platforms: POSIX (Linux, Mac, BSDs...)
- Like command-group >=5.0.0.
- Feature: `process-group` (default)

```rust
TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(CreationFlags::NO_WINDOW | CreationFlags::DETACHED)
  .wrap(JobObject::new())
  .spawn()?;
```

For Windows process groups, use `CreationFlags::NEW_PROCESS_GROUP` and/or `JobObject::new()`.

### Process session

- Platforms: POSIX (Linux, Mac, BSDs...)
- Like command-group <5.0.0.
- Feature: `process-session` (default)

### Pseudo terminal (pty)

- Platforms: Linux, Mac
- Feature: `pty`

### Creation flags

- Platforms: Windows
- Like command-group.
- Feature: `creation-flags` (default)

This is a shim to allow setting Windows process creation flags with this API, as otherwise they'd be overwritten.

Note the `CREATE_SUSPENDED` will always be set, as it is required for the crate to function.

```rust
TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(CreationFlags::NO_WINDOW | CreationFlags::DETACHED)
  .wrap(JobObject::new())
  .spawn()?;
```

Or with custom flags:

```rust
TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(CreationFlags::custom(CREATE_NEW_CONSOLE | CREATE_PROTECTED_PROCESS))
  .wrap(JobObject::new())
  .spawn()?;
```

### Kill on drop

- Platforms: all
- Like command-group.
- Feature: `kill-on-drop` (default)

This is a shim to allow wrappers to handle the kill-on-drop flag, as it can't be read from Command.

```rust
let child = TokioCommandWrap::new(Command::new("watch").arg("ls"))
  .wrap(KillOnDrop)
  .wrap(ProcessGroup::leader())
  .spawn()?;
drop(child);
```

### Your own

Implementing a wrapper is done via a set of traits.

## Features

### Frontends

- `std`: **default**, enables the std-based API.
- `tokio1`: **default**, enables the Tokio-based API.

### Wrappers

- `creation-flags`: **default**, enables the [creation flags](#creation-flags) wrapper.
- `job-object`: **default**, enables the [job object](#job-object) wrapper.
- `kill-on-drop`: **default**, enables the [kill on drop](#kill-on-drop) wrapper.
- `process-group`: **default**, enables the [process group](#process-group) wrapper.
- `process-session`: **default**, enables the [process session](#process-session) wrapper.
- `pty`: **default**, enables the [pseudo terminal](#pseudo-terminal-pty) wrapper.
