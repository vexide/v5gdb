use core::ptr;

/// An instruction-set independent CPU instruction.
///
/// Supports both the ARM (32-bit) instruction set and the Thumb (variable length) instruction set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instruction {
    /// An instruction from the ARM32 instruction set.
    Arm(u32),
    /// An short instruction from the Thumb instruction set.
    Thumb16(u16),
    /// A long instruction from the Thumb instruction set.
    ///
    /// The highest 5 bits of the first halfword in a 32-bit thumb instruction are always
    /// in the range `0b11101..=0b11111`.
    Thumb32([u16; 2]),
}

/// Given the first 16 bits of a Thumb instruction, returns whether the instruction is 32-bit.
#[must_use]
pub const fn is_thumb32(halfword: u16) -> bool {
    matches!(halfword >> 11, 0b11101..=0b11111)
}

impl Instruction {
    /// Returns whether this is a thumb instruction.
    #[must_use]
    pub const fn is_thumb(self) -> bool {
        matches!(self, Self::Thumb16(_) | Self::Thumb32(_))
    }

    /// Returns the size of the instruction in bytes.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::Arm(instr) => size_of_val(&instr),
            Self::Thumb16(instr) => size_of_val(&instr),
            Self::Thumb32(instr) => size_of_val(&instr),
        }
    }

    /// Returns the integer representation of the instruction casted to a usize.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        match self {
            Self::Arm(i) => i as usize,
            Self::Thumb16(i) => i as usize,
            Self::Thumb32(i) => bytemuck::must_cast(i),
        }
    }

    /// Reads either a thumb or ARM instruction from the given pointer.
    ///
    /// # Safety
    ///
    /// The address must point to an instruction of the specified type that is valid for volatile
    /// reads.
    #[must_use]
    pub unsafe fn read(addr: *const u32, thumb: bool) -> Self {
        debug_assert!(!addr.is_null());

        if thumb {
            let addr = addr.cast::<u16>();
            assert!(addr.is_aligned());

            let hw1 = unsafe { ptr::read_volatile(addr) };
            if is_thumb32(hw1) {
                let hw2 = unsafe { ptr::read_volatile(addr.add(1)) };
                Self::Thumb32([hw1, hw2])
            } else {
                Self::Thumb16(hw1)
            }
        } else {
            assert!(addr.is_aligned());
            Self::Arm(unsafe { ptr::read_volatile(addr) })
        }
    }

    /// Writes this instruction to the given pointer.
    ///
    /// # Safety
    ///
    /// The address must be valid for writes. The caller must handle flushing the CPU instruction
    /// cache after calling this method.
    pub unsafe fn write_to(self, addr: *mut u32) {
        debug_assert!(!addr.is_null());
        match self {
            Self::Arm(instr) => unsafe {
                core::ptr::write_volatile(addr, instr);
            },
            Self::Thumb16(instr) => unsafe {
                core::ptr::write_volatile(addr.cast(), instr);
            },
            Self::Thumb32(instr) => unsafe {
                core::ptr::write_volatile(addr.cast(), instr);
            },
        }
    }
}
