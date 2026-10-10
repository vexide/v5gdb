use aarch32_cpu::register::{SysReg, SysRegRead, SysRegWrite};
use arbitrary_int::u27;
use bitbybit::bitfield;

#[bitfield(u32, debug)]
pub struct VectorBaseAddressRegister {
    #[bits(5..=31, rw)]
    vector_base_address: u27,
}

impl VectorBaseAddressRegister {
    /// Creates a register value from a pointer to a vector table.
    pub fn new(ptr: *const ()) -> Self {
        let addr = ptr as u32;
        assert!(addr & 0b11111 == 0, "Must be aligned");
        Self::new_with_raw_value(addr)
    }

    /// Returns a pointer to the vector table.
    pub const fn ptr(self) -> *const () {
        (self.vector_base_address().value() << 5) as *const ()
    }

    /// Reads the given value to VBAR.
    pub fn read() -> Self {
        Self::new_with_raw_value(Self::read_raw())
    }

    /// Writes the given value to VBAR.
    ///
    /// # Safety
    ///
    /// The register value must point to a valid and properly aligned vector table.
    pub unsafe fn write(self) {
        unsafe {
            Self::write_raw(self.raw_value());
        }
    }
}

impl SysReg for VectorBaseAddressRegister {
    const CP: u32 = 15;
    const OP1: u32 = 0;
    const CRN: u32 = 12;
    const CRM: u32 = 0;
    const OP2: u32 = 0;
}

impl SysRegRead for VectorBaseAddressRegister {}
impl SysRegWrite for VectorBaseAddressRegister {}
