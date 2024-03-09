#[doc(inline)]
pub use core::{TokioChildWrapper, TokioCommandWrap, TokioCommandWrapper};

#[cfg(all(unix, feature = "process-group"))]
#[doc(inline)]
pub use process_group::{ProcessGroup, ProcessGroupChild};

mod core;

#[cfg(all(unix, feature = "process-group"))]
mod process_group;
