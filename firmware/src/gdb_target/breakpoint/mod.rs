//! Software breakpoint management.

use gdbstub::target::ext::breakpoints::{Breakpoints, HwBreakpointOps, SwBreakpointOps};
use thiserror::Error;

use super::V5Target;
use crate::cpu::cache;

pub mod hardware;
pub mod software;

impl Breakpoints for V5Target {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }
}

impl V5Target {
    /// Enable or disable the triggering of breakpoints.
    pub fn set_breakpoints_ignored(&mut self, ignored: bool) {
        self.breaks_paused = ignored;
        for bkpt in self.breaks.iter_mut().flatten() {
            bkpt.set_enabled(!ignored);
            cache::sync_instruction(bkpt.cache_target());
        }
        // Hardware breakpoints never trigger in abort mode.
    }
}

#[derive(Debug, Error, PartialEq, Eq, Clone, Copy)]
pub enum BreakpointError {
    /// The region is not writable.
    #[error("software breakpoints cannot be placed in read-only memory")]
    CannotWrite,
    /// There is already a breakpoint with this address.
    #[error("a breakpoint already exists at the requested address")]
    AlreadyExists,
    /// There are no free breakpoint slots.
    #[error("there are no more unused breakpoint slots")]
    NoSpace,
    /// The specified breakpoint address is not aligned properly for the given instruction type.
    #[error("the breakpoint address is not aligned properly for the given instruction type")]
    NotAlignedCorrectly,
}
