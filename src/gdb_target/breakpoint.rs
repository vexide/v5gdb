//! Software breakpoint management.

use gdbstub::target::{
    TargetResult,
    ext::breakpoints::{Breakpoints, HwBreakpoint, HwBreakpointOps, SwBreakpoint, SwBreakpointOps},
};
use snafu::Snafu;

use super::{
    V5Target,
    cache::{self, CacheTarget},
};
use crate::{gdb_target::arch::{ArmBreakpointKind, hw::Specificity}, instruction::Instruction};

/// A software breakpoint.
#[derive(Debug, Clone, Copy)]
pub struct SWBreakpoint {
    enabled: bool,
    instr_addr: u32,
    instr_backup: Instruction,
}

impl SWBreakpoint {
    /// Encoding of an ARM32 `bkpt` instruction.
    pub const ARM_INSTR: Instruction = Instruction::Arm(0xE120_0070);
    /// Encoding of a Thumb `bkpt` instruction.
    pub const THUMB_INSTR: Instruction = Instruction::Thumb(0xBE00);

    /// Create a new software breakpoint targeting the given address.
    ///
    /// The breakpoint will not be initially enabled.
    ///
    /// # Safety
    ///
    /// The address specified must be valid for reads and writes and must be the of the specified
    /// instruction set.
    pub unsafe fn new(addr: u32, thumb: bool) -> Self {
        Self {
            enabled: false,
            instr_addr: addr,
            instr_backup: unsafe { Instruction::read(addr as *const u32, thumb) },
        }
    }

    /// Returns the address that this software breakpoint is active at.
    pub fn address(&self) -> u32 {
        self.instr_addr
    }

    /// Returns whether the breakpoint is currently active.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Enables or disables the specified breakpoint by overwriting the targeted instruction either
    /// with a `bkpt` instruction or the original instruction.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;

        if enabled {
            // If the old instruction was Thumb, then our `bkpt` replacement needs to be Thumb too.
            let bkpt_instr = match self.instr_backup {
                Instruction::Arm(_) => Self::ARM_INSTR,
                Instruction::Thumb(_) => Self::THUMB_INSTR,
            };

            unsafe {
                bkpt_instr.write_to(self.instr_addr as *mut u32);
            }
        } else {
            unsafe {
                self.instr_backup.write_to(self.instr_addr as *mut u32);
            }
        }
    }

    /// Returns the cache target for this breakpoint's instruction.
    #[must_use]
    pub const fn cache_target(&self) -> CacheTarget {
        CacheTarget::Address(self.instr_addr)
    }
}

impl Breakpoints for V5Target {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }
}

impl SwBreakpoint for V5Target {
    fn add_sw_breakpoint(
        &mut self,
        addr: u32,
        kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        let is_arm = matches!(kind, ArmBreakpointKind::Arm32);
        let result = unsafe { self.register_sw_breakpoint(addr, !is_arm) };

        Ok(result.is_ok())
    }

    fn remove_sw_breakpoint(
        &mut self,
        addr: u32,
        _kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        let changed = self.remove_sw_breakpoint(addr);
        Ok(changed)
    }
}

impl HwBreakpoint for V5Target {
    fn add_hw_breakpoint(
        &mut self,
        addr: u32,
        kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        if self.hw_manager.breakpoints_available() <= 1 {
            // One hardware breakpoint should be saved for single stepping.
            return Ok(false);
        }

        let result = self
            .hw_manager
            .add_breakpoint_at(addr, Specificity::Match, kind);

        Ok(result.is_ok())
    }

    fn remove_hw_breakpoint(
        &mut self,
        addr: u32,
        kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        let did_remove = self
            .hw_manager
            .remove_breakpoint_at(addr, Specificity::Match, kind);

        Ok(did_remove)
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

        let mut bkpt = unsafe { SWBreakpoint::new(addr, thumb) };

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
}
