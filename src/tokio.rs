#[doc(inline)]
pub use core::{TokioChildWrapper, TokioCommandWrap, TokioCommandWrapper};
#[cfg(all(unix, feature = "process-group"))]
#[doc(inline)]
pub use process_group::{ProcessGroup, ProcessGroupChild};
#[cfg(all(unix, feature = "process-session"))]
#[doc(inline)]
pub use process_session::ProcessSession;

mod core;
#[cfg(all(unix, feature = "process-group"))]
mod process_group;
#[cfg(all(unix, feature = "process-session"))]
mod process_session;
