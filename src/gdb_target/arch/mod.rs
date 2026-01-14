use std::num::NonZeroUsize;

use cortex_ar::asm::{dsb, isb};
use critical_section::CriticalSection;
use gdbstub::arch::{Arch, RegId, Registers};

use crate::regs::mem::{DomainAccessControlRegister, DomainPermission};

/// The ARMv7 architecture.
pub enum ArmV7 {}

impl Arch for ArmV7 {
    type Usize = u32;
    type BreakpointKind = ArmBreakpointKind;
    type RegId = ArmRegisterID;
    type Registers = ArmRegisters;

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

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ArmRegisters {
    /// General purpose registers (R0-R12)
    pub r: [u32; 13],
    /// Stack Pointer (R13)
    pub sp: u32,
    /// Link Register (R14)
    pub lr: u32,
    /// Program Counter (R15)
    pub pc: u32,
    /// Current Program Status Register (cpsr)
    pub cpsr: u32,
    /// Floating-point/SIMD registers (d0-d31)
    pub d: [u64; 32],
    /// Floating-point status and control register
    pub fpscr: u32,
}

impl Registers for ArmRegisters {
    type ProgramCounter = u32;

    fn pc(&self) -> Self::ProgramCounter {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        let mut send = move |bytes: &[u8]| {
            for &b in bytes {
                write_byte(Some(b));
            }
        };

        for r in self.r {
            send(&r.to_le_bytes());
        }

        send(&self.sp.to_le_bytes());
        send(&self.lr.to_le_bytes());
        send(&self.pc.to_le_bytes());
        send(&self.cpsr.to_le_bytes());

        for d in self.d {
            send(&d.to_le_bytes());
        }

        send(&self.fpscr.to_le_bytes());
    }

    fn gdb_deserialize(&mut self, mut bytes: &[u8]) -> Result<(), ()> {
        fn read<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], ()> {
            let Some((left, right)) = bytes.split_at_checked(N) else {
                return Err(());
            };
            *bytes = right;

            Ok(<[u8; N]>::try_from(left).unwrap())
        }

        for r in &mut self.r {
            *r = u32::from_le_bytes(read(&mut bytes)?);
        }

        self.sp = u32::from_le_bytes(read(&mut bytes)?);
        self.lr = u32::from_le_bytes(read(&mut bytes)?);
        self.pc = u32::from_le_bytes(read(&mut bytes)?);
        self.cpsr = u32::from_le_bytes(read(&mut bytes)?);

        for d in &mut self.d {
            *d = u64::from_le_bytes(read(&mut bytes)?);
        }

        self.fpscr = u32::from_le_bytes(read(&mut bytes)?);

        Ok(())
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
            Self::Gpr(_) => size_of::<u32>(),
            Self::Sp => size_of::<u32>(),
            Self::Lr => size_of::<u32>(),
            Self::Pc => size_of::<u32>(),
            Self::Cpsr => size_of::<u32>(),
            Self::Fpr(_) => size_of::<u64>(),
            Self::Fpscr => size_of::<u32>(),
        })
        .unwrap()
    }
}

impl RegId for ArmRegisterID {
    fn from_raw_id(id: usize) -> Option<(Self, Option<std::num::NonZeroUsize>)> {
        let reg = match id {
            0..=12 => Self::Gpr(id as u8),
            13 => Self::Sp,
            14 => Self::Lr,
            15 => Self::Pc,
            16 => Self::Cpsr,
            17..=49 => Self::Fpr((id - 17) as u8),
            50 => Self::Fpscr,
            _ => return None,
        };

        Some((reg, Some(reg.size())))
    }
}

/// Temporarily disable MMU permission checks, execute a function in a critical section,
/// then restore previous state.
///
/// Since a misbehaving program could corrupt device configuration, VEXos protects some lower-level
/// config registers against accidental writes. This function can be used as a marker for
/// "This memory is being accessed intentionally."
#[inline]
pub fn access_protected_mmio<T>(_cs: CriticalSection<'_>, inner: impl FnOnce() -> T) -> T {
    // Each VMSA region is assigned a domain from 0-15. When a memory access happens, it reads this
    // register to decide whether or not a permission check should be done. If the domain assigned
    // to the given memory region is in Manager mode, no permission check is done.
    let domain_access = DomainAccessControlRegister::read();
    domain_access.set_all(DomainPermission::Manager).write();
    isb(); // Wait for domain permissions change to finish.

    let res = inner();
    dsb(); // Wait for device memory changes to finish.

    // Restore previous state.
    domain_access.write();
    isb(); // Wait for domain permissions change to finish.

    res
}
