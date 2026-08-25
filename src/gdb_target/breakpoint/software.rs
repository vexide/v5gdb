use gdbstub::target::TargetResult;

use crate::{
    cpu::{
        cache::{self, CacheTarget},
        instruction::Instruction,
    },
    gdb_target::{V5Target, arch::ArmBreakpointKind, breakpoint::BreakpointError, memory::test_access},
};

/// A software breakpoint.
#[derive(Debug, Clone, Copy)]
pub struct SwBreakpoint {
    /// Indicates whether the breakpoint can currently be triggered.
    pub enabled: bool,
    /// The target address of the breakpoint.
    pub instr_addr: u32,
    /// The instruction that was originally at [`Self::instr_addr`].
    pub instr_backup: Instruction,
    /// Keeps track of what this breakpoint is being used for.
    pub reason: BreakpointReason,
}

impl SwBreakpoint {
    /// Encoding of an ARM32 `bkpt` instruction.
    pub const ARM_INSTR: Instruction = Instruction::Arm(0xE120_0070);
    /// Encoding of a Thumb `bkpt` instruction.
    pub const THUMB_INSTR: Instruction = Instruction::Thumb16(0xBE00);

    /// Create a new software breakpoint targeting the given address.
    ///
    /// The breakpoint will not be initially enabled.
    ///
    /// # Safety
    ///
    /// The address specified must be valid for reads and writes and must be the of the specified
    /// instruction set.
    pub unsafe fn new(addr: u32, thumb: bool, internal: bool) -> Self {
        Self {
            enabled: false,
            instr_addr: addr,
            instr_backup: unsafe { Instruction::read(addr as *const u32, thumb) },
            reason: BreakpointReason::new(internal),
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
            let bkpt_instr = if self.instr_backup.is_thumb() {
                Self::THUMB_INSTR
            } else {
                Self::ARM_INSTR
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

impl V5Target {
    /// Registers and enables a new software breakpoint.
    ///
    /// If this breakpoint is marked as internal, it will not be shown to users by default.
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
        internal: bool,
    ) -> Result<(), BreakpointError> {
        let mut next_inactive = None;

        for bkpt_slot in self.breaks.iter_mut() {
            if let Some(bkpt) = bkpt_slot
                && bkpt.instr_addr == addr
            {
                // Breakpoint already exists. Either enable the internal/user flag or report an
                // error.

                if internal && !bkpt.reason.internal {
                    bkpt.reason.internal = true;
                } else if !internal && !bkpt.reason.user {
                    bkpt.reason.user = true;
                } else {
                    return Err(BreakpointError::AlreadyExists);
                }

                return Ok(());
            }

            if bkpt_slot.is_none() && next_inactive.is_none() {
                next_inactive = Some(bkpt_slot);
            }
        }

        let Some(bkpt_slot) = next_inactive else {
            return Err(BreakpointError::NoSpace);
        };

        let mut bkpt = unsafe { SwBreakpoint::new(addr, thumb, internal) };

        let size = bkpt.instr_backup.size();

        if test_access(addr..(addr + size as u32), true) != size {
            return Err(BreakpointError::CannotWrite);
        }

        if !self.breaks_paused {
            bkpt.set_enabled(true);
            cache::sync_instruction(bkpt.cache_target());
        }

        *bkpt_slot = Some(bkpt);

        Ok(())
    }

    /// Removes all registered software breakpoints at the given address.
    ///
    /// Returns whether any changes were made.
    pub fn remove_sw_breakpoint(&mut self, addr: u32, internal: bool) -> bool {
        let mut changes_made = false;
        for bkpt_slot in &mut self.breaks {
            let Some(bkpt) = bkpt_slot else {
                continue;
            };

            if bkpt.address() == addr {
                if internal {
                    bkpt.reason.internal = false;
                } else {
                    bkpt.reason.user = false;
                }
            }

            // If neither the user or debugger want this breakpoint anymore, remove it.
            if bkpt.reason.unwanted() {
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

impl gdbstub::target::ext::breakpoints::SwBreakpoint for V5Target {
    fn add_sw_breakpoint(
        &mut self,
        addr: u32,
        kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        let is_arm = matches!(kind, ArmBreakpointKind::Arm32);
        let did_add = unsafe { self.register_sw_breakpoint(addr, !is_arm, false).is_ok() };

        Ok(did_add)
    }

    fn remove_sw_breakpoint(
        &mut self,
        addr: u32,
        _kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        let changed = self.remove_sw_breakpoint(addr, false);
        Ok(changed)
    }
}

/// Indicates why a breakpoint exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakpointReason {
    /// The breakpoint was explicitly requested by the user.
    pub user: bool,
    /// The breakpoint is used internally by the debugger to hook into system functionality.
    pub internal: bool,
}

impl BreakpointReason {
    pub const fn new(internal: bool) -> Self {
        Self {
            internal,
            user: !internal,
        }
    }

    /// Returns whether the breakpoint should be removed.
    pub fn unwanted(&self) -> bool {
        !self.user && !self.internal
    }
}
