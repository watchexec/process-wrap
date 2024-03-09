use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;

use super::TokioCommandWrapper;

#[derive(Debug, Clone)]
pub struct CreationFlags(pub PROCESS_CREATION_FLAGS);

impl TokioCommandWrapper for CreationFlags {}
