use core::num::NonZeroUsize;

use gdbstub::arch::{Arch, RegId};

use crate::exceptions::DebugEventContext;

/// The ARMv7 architecture.
pub enum ArmV7 {}

impl Arch for ArmV7 {
    type Usize = u32;
    type BreakpointKind = ArmBreakpointKind;
    type RegId = ArmRegisterID;
    type Registers = DebugEventContext;

    fn target_description_xml() -> Option<&'static str> {
        Some(include_str!("./target.full.xml"))
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

/// 32-bit ARM register identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmRegisterID {
    /// General purpose registers (R0-R12)
    Gpr(u8),
    /// Stack Pointer (R13)
    Sp,
    /// Link Register (R14)
    Lr,
    /// Program Counter (R15)
    Pc,
    /// Current Program Status Register (cpsr)
    Cpsr,
    /// Floating-point/SIMD registers (F0-F31)
    Fpr(u8),
    /// Floating point status and control register
    Fpscr,
}

impl ArmRegisterID {
    #[must_use]
    const fn size(self) -> NonZeroUsize {
        NonZeroUsize::new(match self {
            Self::Fpr(_) => size_of::<u64>(),
            _ => size_of::<u32>(),
        })
        .unwrap()
    }
}

impl RegId for ArmRegisterID {
    fn from_raw_id(id: usize) -> Option<(Self, Option<core::num::NonZeroUsize>)> {
        let reg = match id {
            0..=12 => Self::Gpr(id as u8),
            13 => Self::Sp,
            14 => Self::Lr,
            15 => Self::Pc,
            25 => Self::Cpsr,
            26..=57 => Self::Fpr((id - 26) as u8),
            58 => Self::Fpscr,
            _ => return None,
        };

        Some((reg, Some(reg.size())))
    }
}
