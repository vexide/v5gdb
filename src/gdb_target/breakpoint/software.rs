use gdbstub::target::TargetResult;

use crate::{
    cache::CacheTarget,
    gdb_target::{V5Target, arch::ArmBreakpointKind},
    instruction::Instruction,
};

/// A software breakpoint.
#[derive(Debug, Clone, Copy)]
pub struct SwBreakpoint {
    pub enabled: bool,
    pub instr_addr: u32,
    pub instr_backup: Instruction,
}

impl SwBreakpoint {
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

impl gdbstub::target::ext::breakpoints::SwBreakpoint for V5Target {
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
