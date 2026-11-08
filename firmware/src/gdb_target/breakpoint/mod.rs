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
    ///
    /// While breakpoints are ignored, both software and hardware breakpoints are removed from the
    /// CPU. Adding or removing breakpoints only updates v5gdb's records, and the changes are
    /// applied when breakpoints are re-enabled. This keeps code that runs while the debug monitor
    /// is active (such as transport callbacks or VEXos tasks) from triggering a recursive debug
    /// event, even if it switches out of Abort mode.
    ///
    /// # Panics
    ///
    /// The caller must ensure that the hardware debug registers are unlocked.
    pub fn set_breakpoints_ignored(&mut self, ignored: bool) {
        self.breaks_paused = ignored;
        for bkpt in self.breaks.iter_mut().flatten() {
            bkpt.set_enabled(!ignored);
            cache::sync_instruction(bkpt.cache_target());
        }
        self.hw_manager.set_paused(ignored);
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
