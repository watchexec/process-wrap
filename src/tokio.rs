#[doc(inline)]
pub use core::{TokioChildWrapper, TokioCommandWrap, TokioCommandWrapper};
#[cfg(all(windows, feature = "creation-flags"))]
#[doc(inline)]
pub use creation_flags::CreationFlags;
#[doc(inline)]
pub use kill_on_drop::KillOnDrop;
#[cfg(all(unix, feature = "process-group"))]
#[doc(inline)]
pub use process_group::{ProcessGroup, ProcessGroupChild};
#[cfg(all(unix, feature = "process-session"))]
#[doc(inline)]
pub use process_session::ProcessSession;

mod core;
#[cfg(all(windows, feature = "creation-flags"))]
mod creation_flags;
mod kill_on_drop;
#[cfg(all(unix, feature = "process-group"))]
mod process_group;
#[cfg(all(unix, feature = "process-session"))]
mod process_session;
