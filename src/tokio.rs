#[doc(inline)]
pub use core::{TokioChildWrapper, TokioCommandWrap, TokioCommandWrapper};
#[cfg(all(windows, feature = "creation-flags"))]
#[doc(inline)]
pub use creation_flags::CreationFlags;
#[cfg(all(windows, feature = "job-object"))]
#[doc(inline)]
pub use job_object::JobObject;
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
#[cfg(all(windows, feature = "job-object"))]
mod job_object;
mod kill_on_drop;
#[cfg(all(unix, feature = "process-group"))]
mod process_group;
#[cfg(all(unix, feature = "process-session"))]
mod process_session;
