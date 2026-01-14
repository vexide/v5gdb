//! Access to Virtual Memory System Architecture registers.

use bitbybit::bitenum;
use cortex_ar::register::{SysReg, SysRegRead, SysRegWrite};

/// The register for controlling permission overrides for each memory domain.
///
/// It's a packed array of 16 `u2` ([`DomainPermission`]) enum variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainAccessControlRegister(pub u32);

impl SysReg for DomainAccessControlRegister {
    const CP: u32 = 15;
    const OP1: u32 = 0;
    const CRN: u32 = 3;
    const CRM: u32 = 0;
    const OP2: u32 = 0;
}

impl SysRegRead for DomainAccessControlRegister {}
impl SysRegWrite for DomainAccessControlRegister {}

impl DomainAccessControlRegister {
    #[must_use]
    pub fn read() -> Self {
        Self(unsafe { <Self as SysRegRead>::read_raw() })
    }

    /// Sets all domains to have the given access control setting.
    #[must_use]
    pub const fn set_all(mut self, permission: DomainPermission) -> Self {
        let bits = permission.raw_value().value() as u32;
        self.0 = 0;
        let mut offset = 0;
        while offset < 32 {
            self.0 |= bits << offset;
            offset += 2;
        }
        self
    }

    pub fn write(self) {
        unsafe { Self::write_raw(self.0); }
    }
}

#[bitenum(u2, exhaustive = false)]
pub enum DomainPermission {
    /// All access to this memory fails.
    NoPermission = 0b00,
    /// Permissions are checked.
    Client = 0b01,
    /// Permissions are not checked.
    Manager = 0b11,
}
