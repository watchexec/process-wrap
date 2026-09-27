//! Composable process command wrappers.
//!
//! # Quick start
//!
//! ```toml
//! [dependencies]
//! process-wrap = { version = "10.0.0", features = ["std"] }
//! ```
//!
//! ```rust,no_run
//! # #[cfg(feature = "std")]
//! # mod example {
//! # fn run() -> std::io::Result<()> {
//! use process_wrap::std::*;
//!
//! let mut command = Command::with_new("watch", |command| { command.arg("ls"); });
//! #[cfg(all(unix, feature = "process-group"))] { command.wrap(ProcessGroup::leader()); }
//! #[cfg(all(windows, feature = "job-object"))] { command.wrap(JobObject); }
//! let mut child = command.spawn()?;
//! let status = child.wait()?;
//! dbg!(status);
//! # Ok(()) }
//! # }
//! # fn main() {}
//! ```
//!
//! ## Migrating from command-group
//!
//! The above example is equivalent to the `command-group` 5.x usage. To migrate from versions 4.x
//! and below, replace `ProcessGroup::leader()` with `ProcessSession`.
//!
//! # Overview
//!
//! This crate provides a composable process-wrap-owned [`Command`] configuration shared by the std
//! and Tokio frontends. It is a more flexible and composable successor to the `command-group` crate,
//! and is meant to be adaptable to additional use cases. The optional Tokio PTY provider demonstrates
//! that adaptability by keeping terminal process creation in the same wrapper lifecycle as process
//! groups, sessions, signal policy, and custom wrappers.
//!
//! # Usage
//!
//! The core APIs are `process_wrap::std::Command` and `process_wrap::tokio::Command`. Both are
//! aliases for one backend-typed command family: construction and configuration are shared, while
//! spawning and child behavior use the selected frontend. `CommandWrap` remains an alias in both
//! modules for compatibility.
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # mod example {
//! use process_wrap::std::*;
//! # fn run() {
//! let mut command = Command::new("ls");
//! command.arg("-l");
//! #[cfg(all(unix, feature = "process-group"))] { command.wrap(ProcessGroup::leader()); }
//! #[cfg(all(windows, feature = "job-object"))] { command.wrap(JobObject); }
//! # }
//! # }
//! # fn main() {}
//! ```
//!
//! The closure constructor remains available, and its inferred argument is now process-wrap's
//! command:
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # mod example {
//! use process_wrap::std::*;
//! # fn run() {
//! let mut command = Command::with_new("ls", |command| { command.arg("-l"); });
//! #[cfg(all(unix, feature = "process-group"))] { command.wrap(ProcessGroup::leader()); }
//! #[cfg(all(windows, feature = "job-object"))] { command.wrap(JobObject); }
//! # }
//! # }
//! # fn main() {}
//! ```
//!
//! Existing native commands can still be converted with `Command::from`. They retain exact native
//! behavior but are native-only: alternate portable transports cannot reconstruct arbitrary native
//! state. `native_mut()` is the explicit mutable escape hatch, and `into_native()` consumes the
//! process-wrap command when the rest of its lifecycle belongs to the native API. Stable native
//! configuration methods remain on the facade where their behavior can be preserved and make the
//! command native-only. The standard library's supplementary-groups setter remains unstable and is
//! not mirrored by the facade. Tokio's process-group setter is likewise omitted at the declared Tokio
//! floor because it cannot exactly replace process-group state already stored in a native-only Tokio
//! command. Use `ProcessGroup` for tracked Tokio commands, or configure a `std::process::Command`
//! before converting it into Tokio and process-wrap. An immutable Tokio `as_std()` view requires the
//! explicit `native_mut().as_std()` transition; Tokio 1.38.2 has no mutable inner-std accessor.
//!
//! If targetting a single platform, then a fluent style is possible:
//!
//! ```rust
//! # #[cfg(all(unix, feature = "std", feature = "process-group"))]
//! # mod example {
//! use process_wrap::std::*;
//! # fn run() {
//! Command::with_new("ls", |command| { command.arg("-l"); })
//!    .wrap(ProcessGroup::leader());
//! # }
//! # }
//! # fn main() {}
//! ```
//!
//! The `wrap` method can be called multiple times to add multiple wrappers. The order of the
//! wrappers can be important, as they are applied in the order they are added. The documentation
//! for each wrapper will specify ordering concerns.
//!
//! The `spawn` method is used to spawn the process, after which the `Child` can be interacted with.
//! Methods on `Child` mimic those on `process::Child`, but may be customised by the wrappers. For
//! example, `kill` will send a signal to the process group if the `ProcessGroup` wrapper is used.
//!
//! # Pseudo-terminals
//!
//! The non-default `pty` feature selects the Tokio frontend and terminal dependencies. It provides
//! native transport on Linux, Android, macOS, FreeBSD, NetBSD 10 and newer, OpenBSD, DragonFly BSD,
//! illumos, Solaris, and Windows 11 24H2 (build 26100) or Windows Server 2025. The Windows backend
//! uses native ConPTY. That Windows floor is required because descendant-aware output EOF requires
//! `ReleasePseudoConsole`. Unavailable platforms report `std::io::ErrorKind::Unsupported`.
//!
//! ```rust,no_run
//! # #[cfg(feature = "pty")]
//! # mod example {
//! # fn run() -> std::io::Result<()> {
//! use process_wrap::tokio::{Command, Pty};
//!
//! #[cfg(unix)]
//! let mut command = Command::with_new("sh", |command| {
//!     command.args(["-c", "printf terminal"]);
//! });
//! #[cfg(windows)]
//! let mut command = Command::with_new("cmd.exe", |command| {
//!     command.args(["/d", "/s", "/c", "echo terminal"]);
//! });
//! command.wrap(Pty::default());
//! let mut child = command.spawn()?;
//! let controller = child
//!     .take_pty_controller()
//!     .expect("a successful PTY spawn installs one controller");
//! # drop(controller);
//! # Ok(()) }
//! # }
//! # fn main() {}
//! ```
//!
//! On every supported platform, including Windows, register `Pty` with the same
//! `Command::wrap(Pty::default()).spawn()` API and take the same `PtyController`. Its ownership and
//! lifecycle contract below applies on Unix and Windows alike. To select a PTY conditionally, use
//! `Pty::check_supported()` for a capability result or `Pty::is_supported()` for a boolean. These
//! report platform and runtime capability only; they do not suppress later configuration,
//! compatibility, or spawn errors.
//!
//! ## Migrating from the former PTY prototype
//!
//! Move command and terminal configuration to the shared Tokio `Command` and `Pty` values. Register
//! that `Pty` as the spawn provider with `.wrap(Pty::default())` (or `.wrap(configured_pty)`), then
//! use ordinary `.spawn()` and its ordinary boxed-child result. Call `take_pty_controller()` once on
//! that returned child to obtain terminal I/O and resize control.
//!
//! On Windows, the ConPTY provider rejects direct `.bat` and `.cmd` programs case-insensitively.
//! Invoke `cmd.exe` explicitly with arguments such as `/d /s /c ...` for shell or batch semantics.
//! The provider resolves executable entries itself and reports unresolved bare programs as `NotFound`;
//! it does not perform implicit shell or `PATHEXT` dispatch.
//!
//! A terminal has one ordered output stream, so PTY standard output and standard error are merged.
//! Input and output each strongly own the logical bidirectional controller; resize handles are weak.
//! Dropping one I/O side is not a half-close. After both are gone, a valid resize request returns
//! `BrokenPipe`. On Windows, final strong-owner loss schedules `ClosePseudoConsole` on a detached
//! worker. If worker creation fails, process-wrap intentionally retains the HPCON rather than
//! potentially blocking the dropping thread, so logical closure does not guarantee OS pseudoconsole
//! teardown. Send VEOF when terminal input policy calls for end-of-file. Waiting for the direct child
//! and draining terminal output are separate lifecycles because descendants may retain the slave. On
//! macOS they should run concurrently while the kernel drains and revokes the terminal during
//! session-leader teardown.
//!
//! The transport owns terminal bytes and resize, not parent-terminal raw mode, relaying, key handling,
//! VT parsing, scrollback, or pager policy. A bare PTY creates its required session.
//! `ProcessGroup::leader()` or `ProcessSession` may independently add group-wide signalling while the
//! direct child is live; waiting continues to follow that child. Attaching to an existing group or
//! explicitly registering both is invalid. `ResetSigmask` composes. Without `JobObject`, Tokio
//! `KillOnDrop` targets only the direct child. On Windows, combining `KillOnDrop` with `JobObject`
//! enables kill-on-last-job-handle-close for every process still associated with the job; either
//! registration order works and the Windows PTY provider preserves that policy. Consuming the
//! `JobObject` layer relinquishes whole-job supervision, leaving only the lower direct-child policy.
//! The returned boxed child keeps arbitrary outer wrappers, and `take_pty_controller()` traverses them
//! and yields the controller once.
//!
//! # KillOnDrop and CreationFlags
//!
//! Calling native `.kill_on_drop()` or `.creation_flags()` makes a command native-only: those
//! settings cannot be queried or reconstructed by wrappers and alternate transports. `JobObject`
//! and spawn providers nevertheless need those policies in order to compose correctly. The
//! `KillOnDrop` and `CreationFlags` wrappers therefore record portable policy on each spawn attempt
//! and _should_ be used instead of the native-only methods when composition is required.
//!
//! In practice:
//!
//! ## Instead of `.kill_on_drop(true)` (Tokio-only):
//!
//! ```rust
//! # #[cfg(all(feature = "tokio1", feature = "kill-on-drop"))]
//! # mod example {
//! use process_wrap::tokio::*;
//! # fn run() {
//! let mut command = Command::with_new("ls", |command| { command.arg("-l"); });
//! command.wrap(KillOnDrop);
//! # }
//! # }
//! # fn main() {}
//! ```
//!
//! ## Instead of `.creation_flags(CREATE_NO_WINDOW)` (Windows-only):
//!
//! ```rust,no_run
//! # #[cfg(all(windows, feature = "std", feature = "creation-flags"))]
//! # fn run() -> std::io::Result<()> {
//! use process_wrap::std::{Command, CreationFlags};
//! use windows::Win32::System::Threading::CREATE_NO_WINDOW;
//!
//! let mut command = Command::with_new("ls", |command| { command.arg("-l"); });
//! command.wrap(CreationFlags(CREATE_NO_WINDOW));
//! # let _child = command.spawn()?;
//! # Ok(()) }
//! # fn main() {}
//! ```
//!
//! Internally the `JobObject` wrapper always sets the `CREATE_SUSPENDED` flag, but as it is able to
//! access the `CreationFlags` value it will either resume the process after setting up, or leave it
//! suspended if `CREATE_SUSPENDED` was explicitly set. `CreationFlags` and `JobObject` may be
//! registered in either order.
//!
//! # Extension
//!
//! The crate is designed to be extensible, and new wrappers can be added by implementing the
//! required traits. Command configuration is shared, but std and Tokio wrapper traits remain
//! separate because their spawn and child APIs differ. Re-use shared policy code when implementing
//! both frontends.
//!
//! At minimum, you must implement `process_wrap::std::CommandWrapper` and/or
//! `process_wrap::tokio::CommandWrapper`. These provide the same functionality
//! (and indeed internally are generated using a common macro), but differ in the exact types used.
//! Here's the most basic impl (shown for Tokio):
//!
//! ```rust
//! # #[cfg(feature = "tokio1")]
//! # mod example {
//! use process_wrap::tokio::*;
//! #[derive(Debug)]
//! pub struct YourWrapper;
//! impl CommandWrapper for YourWrapper {}
//! # }
//! # fn main() {}
//! ```
//!
//! The trait provides extension or hook points into the lifecycle of a `Command`:
//!
//! - **`fn extend(&mut self, other: Self)`** is called if `.wrap(YourWrapper)` is done twice.
//!   Only one wrapper of a given type can exist, so this gives the stored instance an opportunity to
//!   incorporate all or part of the second, concretely typed wrapper. By default, this does nothing
//!   (that is, only the first registered wrapper instance of a type applies).
//!
//! - **`fn pre_spawn(&mut self, attempt: &mut SpawnAttempt, command: &Command)`** is called before
//!   spawning. It can record portable policy for this attempt and inspect peer wrappers through
//!   `command`. Mutations copied from a tracked command apply to one attempt; native-only commands
//!   retain native mutations. Calling `attempt.native_mut()` or `stdin`/`stdout`/`stderr` makes a
//!   tracked attempt incompatible with a portable provider. By default does nothing.
//!
//! - **On Windows, `prepare_child`** runs in registration order after transport creation and before
//!   public post-spawn hooks. It constructs fallible per-wrapper prepared state while cleanup remains
//!   armed; the matching weak token is installed into a detached layer during wrapping before that
//!   layer is published to later callbacks.
//!
//! - **`fn post_spawn(&mut self, attempt: &mut SpawnAttempt, child: &mut dyn ChildWrapper, command: &Command)`**
//!   is called after any transport creates its child. The child may be a terminal custom/provider
//!   child with no native value. Changing command settings on `attempt` here cannot configure the
//!   already-created child. By default does nothing.
//!
//! - **`fn wrap_child(&mut self, child: &mut dyn ChildWrapper, command: &Command) -> io::Result<Option<PendingChildWrapper>>`**
//!   is called after all `post_spawn()` hooks. The current child remains borrowed from process-wrap.
//!   To override child methods, return a `PendingChildWrapper` containing a `ChildWrapperLayer` whose
//!   child slot is empty; process-wrap installs the child after the callback returns successfully.
//!   Child wraps run in registration order, so `.wrap(Foo).wrap(Bar)` produces an outer
//!   `Bar(Foo(child))`. By default returns `None` and leaves the child unchanged.
//!
//! - **`fn spawn_provider(&self) -> Option<&dyn SpawnProvider>`** exposes an alternate transport owned
//!   by this wrapper. A provider exposed during selection must remain available throughout the spawn
//!   lifecycle, and only one registered wrapper may expose one. By default returns `None`.
//!
//! Pre-spawn, post-spawn, and child-wrapping hooks all run in registration order and stop at the first
//! error or unwinding panic. The active wrapper remains registered but is temporarily unavailable
//! through `get_wrap`; peer wrappers remain visible.
//!
//! ## Spawn providers
//!
//! A spawn provider replaces process creation while retaining the complete wrapper lifecycle, making
//! custom transports such as PTYs composable with other wrappers. The provider path runs:
//!
//! 1. `check_available`
//! 2. native-only base rejection
//! 3. `validate_command`
//! 4. every `pre_spawn` hook
//! 5. native-only attempt rejection
//! 6. `validate_attempt`
//! 7. provider `spawn`, returning a child and fresh armed transaction
//! 8. on Windows, every fallible prepared-child construction hook
//! 9. every `post_spawn` hook
//! 10. every child-layer description and detached installation, including matching prepared state and
//!     install-time custody validation
//! 11. on Windows, ordinary finalization and cleanup disarm, cleanup-owner validation, and non-owner
//!     disarm while the sole final owner remains armed
//! 12. fallible private return-custody allocation
//! 13. transaction `commit`
//! 14. committed transaction-residue installation
//! 15. on Windows, the sole final cleanup owner disarms
//! 16. infallible private ownership moves and child return
//!
//! Validation rejects unsupported portable policy before operating-system allocation. `spawn`
//! returns a child satisfying the frontend's complete `ChildWrapper` contract and a fresh, armed
//! `SpawnTransaction` which owns cleanup independently of the child chain. On Windows, prepared-state
//! construction occurs before public post hooks. After wrapping and install-time custody validation,
//! ordinary finalization, ordinary cleanup disarm, owner selection, and non-owner disarm all complete
//! before return-custody allocation and commit; every step is fallible while the transaction remains
//! armed. A later error or unwinding panic causes best-effort rollback while preserving the original
//! failure. After capturing that failure, process-wrap first resolves transaction cleanup, then
//! disposes any detached child layer, the child chain, and each Windows prepared value independently.
//! Secondary cleanup panic payloads are quarantined without inspection or destruction. Successful
//! commit ends failed-spawn rollback, and its residue is installed in the already-allocated return
//! custody. On Windows, the sole final owner remains armed until it disarms after commit; no caller
//! callback, allocation-dependent work, or native handle close follows that successful transition
//! before the infallible return move. On a successful spawn, arbitrary residue destruction occurs
//! outside the spawn lifecycle. Committed residue must retain no armed cleanup or independent process,
//! terminal, controller, handle, pseudoconsole, or other liveness resource.
//!
//! Rollback, wrapper restoration, original panic-payload preservation, and cleanup-diagnostic panic
//! containment apply only to unwinding panics. With `panic=abort`, the process terminates before
//! those guarantees can run. Cleanup before `spawn` returns that product remains the provider's
//! responsibility.
//!
//! A command may register only one provider; conflicts are rejected before callbacks or allocation.
//! Providers and wrapper state are reusable across repeated spawns. `spawn_with` and
//! `spawn_with_child` reject a registered provider instead of bypassing it. On Unix, when wrappers
//! request built-in child setup, a successful explicit spawner must create its returned child before
//! replacing the native command. Whenever it replaces that command, including before returning an
//! error or unwinding, the displaced command must be dropped before control leaves the spawner. A
//! replacement is discarded with a tracked attempt or retained by a native-only base. Process-wrap
//! cannot apply setup to a replacement which the closure creates and immediately spawns.
//!
//! ## An Example Logging Wrapper
//!
//! Let's implement a logging wrapper that redirects a `Command`'s `stdout` and `stderr` into a
//! text file. We can use `std::io::pipe` to merge `stdout` and `stderr` into one channel, then
//! `std::io::copy` in a background thread to non-blockingly stream that data to disk as it comes
//! in.
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # mod example {
//! # use process_wrap::std::{CommandWrap, CommandWrapper, SpawnAttempt};
//! # use std::{fs::File, io, path::PathBuf, thread};
//! #[derive(Debug)]
//! struct LogFile {
//!     path: PathBuf,
//! }
//!
//! impl LogFile {
//!     fn new(path: impl Into<PathBuf>) -> Self {
//!         Self { path: path.into() }
//!     }
//! }
//!
//! impl CommandWrapper for LogFile {
//!     fn pre_spawn(&mut self, command: &mut SpawnAttempt, _core: &CommandWrap) -> io::Result<()> {
//!         let mut logfile = File::create(&self.path)?;
//!         let (mut rx, tx) = io::pipe()?;
//!
//!         thread::spawn(move || {
//!          io::copy(&mut rx, &mut logfile).unwrap();
//!         });
//!
//!         command
//!             .stdout(tx.try_clone()?.into())
//!             .stderr(tx.into());
//!         Ok(())
//!     }
//! }
//! # }
//! # fn main() {}
//! ```
//!
//! That is a useful start, but returning from `main` can terminate the process before the detached
//! worker finishes copying and flushing terminal bytes, leaving incomplete log output. Retain the
//! background thread's `JoinHandle` and join it from the child wrapper's `wait` method so copying
//! completes before the caller proceeds.
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # mod example {
//! # use process_wrap::std::{
//! #     ChildWrapper, ChildWrapperLayer, ChildWrapperSlots, Command as WrappedCommand,
//! #     CommandWrap, CommandWrapper, PendingChildWrapper, SpawnAttempt,
//! # };
//! # use std::{
//! #     fs::File,
//! #     io, mem,
//! #     path::PathBuf,
//! #     process::ExitStatus,
//! #     thread::{self, JoinHandle},
//! # };
//! #[derive(Debug)]
//! struct LogFile {
//!     path: PathBuf,
//!     thread: Option<JoinHandle<()>>,
//! }
//!
//! impl LogFile {
//!     fn new(path: impl Into<PathBuf>) -> Self {
//!         Self {
//!          path: path.into(),
//!          thread: None,
//!         }
//!     }
//! }
//!
//! impl CommandWrapper for LogFile {
//!     fn pre_spawn(&mut self, command: &mut SpawnAttempt, _core: &CommandWrap) -> io::Result<()> {
//!         let mut logfile = File::create(&self.path)?;
//!         let (mut rx, tx) = io::pipe()?;
//!
//!         self.thread = Some(thread::spawn(move || {
//!          io::copy(&mut rx, &mut logfile).unwrap();
//!         }));
//!
//!         command
//!             .stdout(tx.try_clone()?.into())
//!             .stderr(tx.into());
//!         Ok(())
//!     }
//!
//!     fn wrap_child(
//!         &mut self,
//!         _child: &mut dyn ChildWrapper,
//!         _core: &CommandWrap,
//!     ) -> io::Result<Option<PendingChildWrapper>> {
//!         let wrapped_child = LogFileChild {
//!          inner: None,
//!          thread: mem::take(&mut self.thread),
//!         };
//!         Ok(Some(PendingChildWrapper::new(wrapped_child)))
//!     }
//! }
//!
//! #[derive(Debug)]
//! struct LogFileChild {
//!     inner: Option<Box<dyn ChildWrapper>>,
//!     thread: Option<JoinHandle<()>>,
//! }
//!
//! impl ChildWrapperLayer for LogFileChild {
//!     fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
//!         ChildWrapperSlots::new(&mut self.inner)
//!     }
//! }
//!
//! impl ChildWrapper for LogFileChild {
//!     fn inner(&self) -> &dyn ChildWrapper {
//!         self.inner.as_deref().expect("an installed log layer owns its child")
//!     }
//!
//!     fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
//!         self.inner.as_deref_mut().expect("an installed log layer owns its child")
//!     }
//!
//!     fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
//!         self.inner.take().expect("an installed log layer owns its child")
//!     }
//!
//!     #[cfg(windows)]
//!     fn process_handle(
//!         &self,
//!     ) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
//!         self.inner().process_handle()
//!     }
//!
//!     fn wait(&mut self) -> io::Result<ExitStatus> {
//!         let exit_status = self.inner_mut().wait();
//!
//!         if let Some(thread) = mem::take(&mut self.thread) {
//!          thread.join().unwrap();
//!         }
//!
//!         exit_status
//!     }
//! }
//! # }
//! # fn main() {}
//! ```
//!
//! Calling `stdout` and `stderr` makes this attempt native-only, so this particular wrapper is for the
//! native or explicit spawning paths. A registered portable provider rejects the opaque attempt before
//! its validation or allocation callbacks.
//!
//! The tracked process-wrap command does not retain the `tx` handles from this hook. Each spawn uses
//! a fresh native attempt command, and that attempt is dropped before `spawn()` returns. The child has
//! already inherited the descriptors it needs, so the background reader sees EOF once the child and
//! its descendants release their copies. Native-only commands retain their native command state by
//! definition; wrappers which install one-attempt resources should therefore use tracked commands.
//!
//! Finally, we can test that our new command-wrapper works:
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # mod example {
//! # use process_wrap::std::{
//! #     ChildWrapper, ChildWrapperLayer, ChildWrapperSlots, Command as WrappedCommand,
//! #     CommandWrap, CommandWrapper, PendingChildWrapper, SpawnAttempt,
//! # };
//! # use std::{
//! #     error::Error,
//! #     fs::{self, File},
//! #     io, mem,
//! #     path::PathBuf,
//! #     process::ExitStatus,
//! #     thread::{self, JoinHandle},
//! # };
//! # use tempfile::NamedTempFile;
//! # #[derive(Debug)]
//! # struct LogFile {
//! #     path: PathBuf,
//! #     thread: Option<JoinHandle<()>>,
//! # }
//! #
//! # impl LogFile {
//! #     fn new(path: impl Into<PathBuf>) -> Self {
//! #         Self {
//! #          path: path.into(),
//! #          thread: None,
//! #         }
//! #     }
//! # }
//! #
//! # impl CommandWrapper for LogFile {
//! #     fn pre_spawn(&mut self, command: &mut SpawnAttempt, _core: &CommandWrap) -> io::Result<()> {
//! #         let mut logfile = File::create(&self.path)?;
//! #         let (mut rx, tx) = io::pipe()?;
//! #
//! #         self.thread = Some(thread::spawn(move || {
//! #          io::copy(&mut rx, &mut logfile).unwrap();
//! #         }));
//! #
//! #         command
//! #             .stdout(tx.try_clone()?.into())
//! #             .stderr(tx.into());
//! #         Ok(())
//! #     }
//! #
//! #     fn wrap_child(
//! #         &mut self,
//! #         _child: &mut dyn ChildWrapper,
//! #         _core: &CommandWrap,
//! #     ) -> io::Result<Option<PendingChildWrapper>> {
//! #         let wrapped_child = LogFileChild {
//! #          inner: None,
//! #          thread: mem::take(&mut self.thread),
//! #         };
//! #         Ok(Some(PendingChildWrapper::new(wrapped_child)))
//! #     }
//! # }
//! #
//! # #[derive(Debug)]
//! # struct LogFileChild {
//! #     inner: Option<Box<dyn ChildWrapper>>,
//! #     thread: Option<JoinHandle<()>>,
//! # }
//! #
//! # impl ChildWrapperLayer for LogFileChild {
//! #     fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_> {
//! #         ChildWrapperSlots::new(&mut self.inner)
//! #     }
//! # }
//! #
//! # impl ChildWrapper for LogFileChild {
//! #     fn inner(&self) -> &dyn ChildWrapper {
//! #         self.inner.as_deref().expect("an installed log layer owns its child")
//! #     }
//! #
//! #     fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
//! #         self.inner.as_deref_mut().expect("an installed log layer owns its child")
//! #     }
//! #
//! #     fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
//! #         self.inner.take().expect("an installed log layer owns its child")
//! #     }
//! #
//! #     #[cfg(windows)]
//! #     fn process_handle(
//! #         &self,
//! #     ) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
//! #         self.inner().process_handle()
//! #     }
//! #
//! #     fn wait(&mut self) -> io::Result<ExitStatus> {
//! #         let exit_status = self.inner_mut().wait();
//! #
//! #         if let Some(thread) = mem::take(&mut self.thread) {
//! #          thread.join().unwrap();
//! #         }
//! #
//! #         exit_status
//! #     }
//! # }
//! #
//! fn main() -> Result<(), Box<dyn Error>> {
//!     #[cfg(windows)]
//!     let mut command = WrappedCommand::with_new("cmd", |command| {
//!         command.args(["/c", "echo Hello && echo World 1>&2"]);
//!     });
//!     #[cfg(unix)]
//!     let mut command = WrappedCommand::with_new("sh", |command| {
//!         command.args(["-c", "echo Hello && echo World 1>&2"]);
//!     });
//!
//!     let logfile = NamedTempFile::new()?;
//!     let logfile_path = logfile.path();
//!
//!     command.wrap(LogFile::new(logfile_path)).spawn()?.wait()?;
//!
//!     let logfile_lines: Vec<String> = fs::read_to_string(logfile_path)?
//!         .lines()
//!         .map(|l| l.trim().into())
//!         .collect();
//!     assert_eq!(logfile_lines, vec!["Hello", "World"]);
//!
//!     Ok(())
//! }
//! # }
//! # fn main() {}
//! ```
//!
//! # Features
//!
//! ## Frontends
//!
//! The default features do not enable a frontend, so you must choose one of the following:
//!
//! - `std`: enables the std-based API.
//! - `tokio1`: enables the Tokio-based API.
//!
//! Both can exist at the same time, but generally you'll want to use one or the other.
//!
//! ## Wrappers
//!
//! - `creation-flags`: **default**, enables the creation flags wrapper (Windows-only).
//! - `job-object`: **default**, enables the job object wrapper (Windows-only).
//! - `kill-on-drop`: **default**, enables the kill on drop wrapper (Tokio-only).
//! - `process-group`: **default**, enables the process group wrapper (Unix-only).
//! - `process-session`: **default**, enables the process session wrapper (Unix-only).
//! - `pty`: enables Tokio pseudo-terminal transport and implies `tokio1`.
//! - `reset-sigmask`: enables the sigmask reset wrapper (Unix-only).
//!
//! ## Diagnostics
//!
//! - `tracing`: **default**, enables internal lifecycle diagnostics through the `tracing` crate.
//!
#![doc(html_favicon_url = "https://watchexec.github.io/logo:command-group.svg")]
#![doc(html_logo_url = "https://watchexec.github.io/logo:command-group.svg")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

#[cfg(all(test, windows))]
pub(crate) mod test_allocator {
	use std::{
		alloc::{GlobalAlloc, Layout, System},
		cell::Cell,
		sync::atomic::{AtomicBool, Ordering},
	};

	#[derive(Default)]
	pub(crate) struct PostTransitionProbe {
		pub(crate) active: AtomicBool,
		pub(crate) transitioned: AtomicBool,
		allocator_callback: AtomicBool,
		operation: AtomicBool,
	}

	impl PostTransitionProbe {
		#[cfg(feature = "job-object")]
		pub(crate) fn arm(&self) {
			assert!(!self.active.swap(true, Ordering::SeqCst));
			self.transitioned.store(false, Ordering::SeqCst);
			self.allocator_callback.store(false, Ordering::SeqCst);
			self.operation.store(false, Ordering::SeqCst);
		}

		pub(crate) fn observe_operation(&self) {
			if self.active.load(Ordering::SeqCst) && self.transitioned.load(Ordering::SeqCst) {
				self.operation.store(true, Ordering::SeqCst);
			}
		}

		fn observe_allocator_callback(&self) {
			if self.active.load(Ordering::SeqCst) && self.transitioned.load(Ordering::SeqCst) {
				self.allocator_callback.store(true, Ordering::SeqCst);
			}
		}

		#[cfg(feature = "job-object")]
		pub(crate) fn finish(&self) -> (bool, bool, bool) {
			self.active.store(false, Ordering::SeqCst);
			(
				self.transitioned.load(Ordering::SeqCst),
				self.allocator_callback.load(Ordering::SeqCst),
				self.operation.load(Ordering::SeqCst),
			)
		}
	}

	#[cfg(feature = "job-object")]
	#[derive(Default)]
	pub(crate) struct SpawnCleanupHandleProbe {
		active: AtomicBool,
		closes: std::sync::atomic::AtomicUsize,
	}

	#[cfg(feature = "job-object")]
	impl SpawnCleanupHandleProbe {
		pub(crate) fn arm(&self) {
			assert!(!self.active.swap(true, Ordering::SeqCst));
			self.closes.store(0, Ordering::SeqCst);
		}

		fn observe_close(&self) {
			if self.active.load(Ordering::SeqCst) {
				self.closes.fetch_add(1, Ordering::SeqCst);
			}
		}

		pub(crate) fn close_count(&self) -> usize {
			self.closes.load(Ordering::SeqCst)
		}

		pub(crate) fn finish(&self) -> usize {
			self.active.store(false, Ordering::SeqCst);
			self.close_count()
		}
	}

	thread_local! {
		static POST_TRANSITION_PROBE: Cell<Option<&'static PostTransitionProbe>> = const { Cell::new(None) };
		#[cfg(feature = "job-object")]
		static SPAWN_CLEANUP_HANDLE_PROBE: Cell<Option<&'static SpawnCleanupHandleProbe>> = const { Cell::new(None) };
	}

	pub(crate) fn current_probe() -> &'static PostTransitionProbe {
		if let Some(probe) = POST_TRANSITION_PROBE.get() {
			return probe;
		}
		let probe = Box::leak(Box::new(PostTransitionProbe::default()));
		POST_TRANSITION_PROBE.set(Some(probe));
		probe
	}

	#[cfg(feature = "job-object")]
	pub(crate) fn current_spawn_cleanup_handle_probe() -> &'static SpawnCleanupHandleProbe {
		if let Some(probe) = SPAWN_CLEANUP_HANDLE_PROBE.get() {
			return probe;
		}
		let probe = Box::leak(Box::new(SpawnCleanupHandleProbe::default()));
		SPAWN_CLEANUP_HANDLE_PROBE.set(Some(probe));
		probe
	}

	#[cfg(feature = "job-object")]
	pub(crate) fn observe_spawn_cleanup_handle_close() {
		let _ = SPAWN_CLEANUP_HANDLE_PROBE.try_with(|slot| {
			if let Some(probe) = slot.get() {
				probe.observe_close();
			}
		});
	}

	fn observe_allocator_callback() {
		let _ = POST_TRANSITION_PROBE.try_with(|slot| {
			if let Some(probe) = slot.get() {
				probe.observe_allocator_callback();
			}
		});
	}

	struct TrackingAllocator;

	// SAFETY: every operation preserves `System`'s arguments and return value exactly. The probe uses
	// only already-initialized thread-local and atomic state and does not alter allocator ownership.
	unsafe impl GlobalAlloc for TrackingAllocator {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			observe_allocator_callback();
			// SAFETY: `layout` is passed through unchanged under `GlobalAlloc::alloc`'s contract.
			unsafe { System.alloc(layout) }
		}

		unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
			observe_allocator_callback();
			// SAFETY: `layout` is passed through unchanged under `GlobalAlloc::alloc_zeroed`'s contract.
			unsafe { System.alloc_zeroed(layout) }
		}

		unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
			observe_allocator_callback();
			// SAFETY: both arguments are passed through unchanged under `GlobalAlloc::dealloc`'s contract.
			unsafe { System.dealloc(pointer, layout) }
		}

		unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
			observe_allocator_callback();
			// SAFETY: all arguments are passed through unchanged under `GlobalAlloc::realloc`'s contract.
			unsafe { System.realloc(pointer, layout, size) }
		}
	}

	#[global_allocator]
	static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;
}

mod command;
pub(crate) mod generic_wrap;
#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
pub(crate) mod unix;
#[cfg(all(unix, any(feature = "std", feature = "tokio1")))]
#[cfg_attr(docsrs, doc(cfg(all(unix, any(feature = "std", feature = "tokio1")))))]
pub use unix::ProcessGroupTarget;

#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
pub use command::WindowsSpawnPolicy;
#[doc(hidden)]
pub use command::{Backend, Blocking, NativeCommand, Tokio1};
pub use command::{Command, CommandArg, SpawnAttempt, SpawnTransaction};

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub mod std;

#[cfg(feature = "tokio1")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio1")))]
pub mod tokio;

#[cfg(all(
	windows,
	feature = "job-object",
	any(feature = "std", feature = "tokio1")
))]
mod windows;

/// Internal memoization of the exit status of a child process.
#[allow(dead_code)] // easier than listing exactly which featuresets use it
#[derive(Clone, Copy, Debug)]
pub(crate) enum ChildExitStatus {
	Running,
	Exited(::std::process::ExitStatus),
}
