use std::arch::asm;

use cortex_ar::{
    asm::{dsb, isb},
    register::Sctlr,
};
use critical_section::CriticalSection;
use gdbstub::arch::Arch;
use gdbstub_arch::arm::reg::{ArmCoreRegs, id::ArmCoreRegId};

pub mod hw;

/// The ARMv7 architecture.
pub enum ArmV7 {}

impl Arch for ArmV7 {
    type Usize = u32;
    type BreakpointKind = ArmBreakpointKind;
    type RegId = ArmCoreRegId;
    type Registers = ArmCoreRegs;

    fn target_description_xml() -> Option<&'static str> {
        Some(include_str!("arch/target.xml"))
    }
}

/// ARM-specific breakpoint kinds.
///
/// Extracted from the GDB documentation at
/// [E.5.1.1 ARM Breakpoint Kinds](https://sourceware.org/gdb/current/onlinedocs/gdb/ARM-Breakpoint-Kinds.html#ARM-Breakpoint-Kinds)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmBreakpointKind {
    /// 16-bit Thumb mode breakpoint.
    Thumb16,
    /// 32-bit Thumb mode (Thumb-2) breakpoint.
    Thumb32,
    /// 32-bit ARM mode breakpoint.
    Arm32,
}

impl gdbstub::arch::BreakpointKind for ArmBreakpointKind {
    fn from_usize(kind: usize) -> Option<Self> {
        let kind = match kind {
            2 => ArmBreakpointKind::Thumb16,
            3 => ArmBreakpointKind::Thumb32,
            4 => ArmBreakpointKind::Arm32,
            _ => return None,
        };
        Some(kind)
    }
}

/// Temporarily disable MMU protections, execute a function in a critical section, then restore
/// previous state.
///
/// # Safety
///
/// This works by disabling the MMU and data-cache to make all memory behave like device memory, but
/// there might still be a dirty cache left over for the memory you are accessing. Therefore, the
/// caller must take care to clean any possible dirty d-cache before accessing any memory.
#[inline]
unsafe fn access_protected_mmio<T>(_cs: CriticalSection<'_>, inner: impl FnOnce() -> T) -> T {
    // FIQs should be off too since their handlers might expect the MMU to work properly.
    unsafe {
        asm!("cpsid f", options(nomem, nostack, preserves_flags));
    }

    let orig_sctlr = Sctlr::read();

    // Wait for pending writes to finish before updating MMU.
    dsb();

    Sctlr::write(
        orig_sctlr
            .with_m(false) // No MMU
            .with_c(false), // No d-cache (write directly to device memory)
    );
    // Wait for SCTLR update to finish.
    isb();

    let res = inner();
    // Wait for device memory to be finished updating.
    dsb();

    Sctlr::write(orig_sctlr);
    // Wait for SCTLR update to finish.
    isb();

    // Re-enable FIQs
    unsafe {
        asm!("cpsie f", options(nomem, nostack, preserves_flags));
    }

    res
}
