use gdbstub::target::TargetError;
use thiserror::Error;

use crate::gdb_target::breakpoint::BreakpointError;

#[derive(Debug, Error)]
pub enum FatalError {
    /// Breakpoint configuration failed.
    #[error("failed to configure breakpoint")]
    Breakpoint(#[from] BreakpointError),
}

impl From<FatalError> for TargetError<FatalError> {
    fn from(e: FatalError) -> Self {
        TargetError::Fatal(e)
    }
}
