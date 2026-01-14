//! Software breakpoint management.

use gdbstub::target::ext::breakpoints::{Breakpoints, HwBreakpointOps, SwBreakpointOps};
use snafu::Snafu;

use super::{
    V5Target,
    cache::{self, CacheTarget},
};
use crate::gdb_target::breakpoint::software::SwBreakpoint;

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
    /// Registers and enables a new software breakpoint.
    ///
    /// This function will handle any required cache invalidation.
    ///
    /// # Errors
    ///
    /// If there is already a breakpoint registered at the given address, returns a
    /// [`BreakpointError::AlreadyExists`] error.
    ///
    /// Returns a [`BreakpointError::NoSpace`] error if there aren't any breakpoints storage slots
    /// available to keep track of this breakpoint.
    ///
    /// # Safety
    ///
    /// The address specified must be valid for reads and writes and must be the of the specified
    /// instruction set.
    pub unsafe fn register_sw_breakpoint(
        &mut self,
        addr: u32,
        thumb: bool,
    ) -> Result<(), BreakpointError> {
        let mut next_inactive = None;

        for bkpt_slot in self.breaks.iter_mut() {
            if let Some(bkpt) = bkpt_slot
                && bkpt.instr_addr == addr
            {
                return Err(BreakpointError::AlreadyExists);
            }

            if bkpt_slot.is_none() && next_inactive.is_none() {
                next_inactive = Some(bkpt_slot);
            }
        }

        let Some(bkpt_slot) = next_inactive else {
            return Err(BreakpointError::NoSpace);
        };

        let mut bkpt = unsafe { SwBreakpoint::new(addr, thumb) };

        bkpt.set_enabled(true);
        cache::sync_instruction(bkpt.cache_target());

        *bkpt_slot = Some(bkpt);

        Ok(())
    }

    /// Removes all registered software breakpoints at the given address.
    ///
    /// Returns whether any changes were made.
    pub fn remove_sw_breakpoint(&mut self, addr: u32) -> bool {
        let mut changes_made = false;
        for bkpt_slot in &mut self.breaks {
            let Some(bkpt) = bkpt_slot else {
                continue;
            };

            if bkpt.address() == addr {
                bkpt.set_enabled(false);
                *bkpt_slot = None;
                changes_made = true;
            }
        }

        if changes_made {
            cache::sync_instruction(CacheTarget::Address(addr));
        }

        changes_made
    }

    /// Returns the index of the tracked software breakpoint at the specified address.
    #[must_use]
    pub fn query_sw_breakpoint(&self, addr: u32) -> Option<usize> {
        self.breaks
            .iter()
            .position(|slot| matches!(slot, Some(bkpt) if bkpt.address() == addr))
    }
}

#[derive(Debug, Snafu)]
pub enum BreakpointError {
    /// There is already a breakpoint with this address.
    AlreadyExists,
    /// There are no free breakpoint slots.
    NoSpace,
    /// The specified breakpoint address is not aligned properly for the given instruction type.
    NotAlignedCorrectly,
}
