#[doc(inline)]
pub use core::{TokioChildWrapper, TokioCommandWrap, TokioCommandWrapper};
#[cfg(all(unix, feature = "process-group"))]
#[doc(inline)]
pub use process_group::{ProcessGroup, ProcessGroupChild};
#[cfg(all(unix, feature = "process-session"))]
#[doc(inline)]
pub use process_session::ProcessSession;
#[cfg(all(windows, feature = "creation-flags"))]
#[doc(inline)]
pub use creation_flags::CreationFlags;

mod core;
#[cfg(all(unix, feature = "process-group"))]
mod process_group;
#[cfg(all(unix, feature = "process-session"))]
mod process_session;
#[cfg(all(windows, feature = "creation-flags"))]
mod creation_flags;
