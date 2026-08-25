//! Access to Virtual Memory System Architecture registers.

use core::arch::asm;

use aarch32_cpu::{
    asm::{dsb, isb},
    mmu::{CachePolicy, MemoryRegionAttributes},
    register::{Par, SysReg, SysRegRead, SysRegWrite},
};
use arbitrary_int::*;
use bitbybit::{bitenum, bitfield};

#[bitfield(u32, debug)]
pub struct TranslationTableBaseControlRegister {
    /// Indicates whether accessing TT0 will be fault if there's a TLB miss.
    #[bit(5, rw)]
    tt1_walk_disable: bool,
    /// Indicates whether accessing TT0 will be fault if there's a TLB miss.
    #[bit(4, rw)]
    tt0_walk_disable: bool,
    /// Indicates how many upper bits of a virtual address must be all zeros for translation table
    /// #0 to be used for a lookup operation.
    ///
    /// For instance, if this is 0, then TT0 is used for all addresses. If this is 4, then
    /// addresses starting with 0b0000… use TT0. This can be useful to make TT1 describe kernel-
    /// space memory and TT0 describe user-space memory.
    ///
    /// When N is higher, the TT0 does not have to be as strongly aligned and it has fewer entries.
    #[bits(0..=2, rw)]
    tt0_boundary: u3,
}

impl SysReg for TranslationTableBaseControlRegister {
    const CP: u32 = 15;
    const OP1: u32 = 0;
    const CRN: u32 = 2;
    const CRM: u32 = 0;
    const OP2: u32 = 2;
}
impl SysRegRead for TranslationTableBaseControlRegister {}
impl SysRegWrite for TranslationTableBaseControlRegister {}

impl TranslationTableBaseControlRegister {
    pub fn read() -> Self {
        Self::new_with_raw_value(Self::read_raw())
    }

    /// Returns whether the given virtual address uses translation table #0.
    pub fn virtual_addr_uses_tt0(self, addr: u32) -> bool {
        let n = self.tt0_boundary().value() as u32;
        addr.leading_zeros() >= n
    }
}

#[bitfield(u32, debug, introspect)]
pub struct TranslationTableBaseRegister {
    #[bits(7..=31, rw)]
    upper_base_addr: u25,
    #[bit(5, rw)]
    not_outer_shareable: bool,
    #[bits(3..=4, rw)]
    outer_cache_attrs: CachePolicy,
    #[bit(1, rw)]
    sharable: bool,
    #[bit(0, rw)]
    cacheable: bool,
}

impl TranslationTableBaseRegister {
    pub fn read_tt0() -> Self {
        let ttbr0: u32;
        unsafe {
            asm!(
                "mrc p15, 0, {}, c2, c0, 0",
                out(reg) ttbr0,
                options(nomem, nostack, preserves_flags)
            );
        }
        Self::new_with_raw_value(ttbr0)
    }

    pub fn read_tt1() -> Self {
        let ttbr1: u32;
        unsafe {
            asm!(
                "mrc p15, 0, {}, c2, c0, 1",
                out(reg) ttbr1,
                options(nomem, nostack, preserves_flags)
            );
        }
        Self::new_with_raw_value(ttbr1)
    }

    pub fn base_ptr(self, tt_boundary: u8) -> *mut L1Descriptor {
        // When tt_boundary is higher, addresses have more leading zeros. Since the MMU calculates
        // the address of a TT entry by ORing the base ptr and the bit-shifted virtual address,
        // the base address can be less strongly aligned in this case.
        let last_addr_bit = 14 - tt_boundary;
        let mask = !0u32 << last_addr_bit;
        let addr = self.raw_value() & mask & Self::upper_base_addr_mask();
        addr as *mut L1Descriptor
    }
}

pub struct TranslationTable {
    /// The number of leading bits in an address which must be all zeros to resolve to this table.
    tt_boundary: u8,
    /// The pointer to the beginning of the table.
    base_ptr: *mut L1Descriptor,
    /// The number of entries in the table.
    num_entries: usize,
}

impl TranslationTable {
    /// Returns the translation table that would be used for the given address.
    pub fn for_addr(addr: u32) -> Self {
        let ctrl = TranslationTableBaseControlRegister::read();
        let is_tt0 = ctrl.virtual_addr_uses_tt0(addr);

        let tt_boundary = if is_tt0 {
            ctrl.tt0_boundary().value()
        } else {
            0 // TT1 lookups always work as if the boundary (TTBCR.N) = 0
        };

        let ttbr = if is_tt0 {
            TranslationTableBaseRegister::read_tt0()
        } else {
            TranslationTableBaseRegister::read_tt1()
        };

        // The trailing 20 bits of an address don't affect which section it's in.
        let section_inner_bits = 20;
        // `tt_boundary` determines how many leading addr bits are unused, so each increment of it
        // halves the size of the translation table's region.
        let num_entries = 2usize.pow(32 - section_inner_bits - tt_boundary as u32);

        Self {
            base_ptr: ttbr.base_ptr(tt_boundary),
            tt_boundary,
            num_entries,
        }
    }

    pub fn base_ptr(&self) -> *const L1Descriptor {
        self.base_ptr
    }

    /// Returns a pointer to the level-1 descriptor for the given address in this translation table.
    pub fn lookup_l1(&self, addr: u32) -> *mut L1Descriptor {
        assert!(addr.leading_zeros() >= self.tt_boundary as u32);

        let section_index = addr as usize >> 20; // Which 1MiB section is this?
        assert!(section_index < self.num_entries);

        unsafe { self.base_ptr.add(section_index) }
    }
}

#[bitfield(u32)]
pub struct L1Descriptor {
    #[bits(0..=1, r)]
    variant: Option<L1DescriptorType>,
}

impl L1Descriptor {
    pub fn as_section(self) -> Option<SectionDescriptor> {
        if self.variant() == Ok(L1DescriptorType::Section) {
            Some(SectionDescriptor::new_with_raw_value(self.raw_value))
        } else {
            None
        }
    }
}

/// The possible descriptor types for a 1MB section of virtual memory.
#[derive(Debug, PartialEq, Eq)]
#[bitenum(u2, exhaustive = false)]
pub enum L1DescriptorType {
    /// This section is unmapped.
    Invalid = 0b00,
    /// Accesses to this section fall back to a set of fine-grained L2 descriptors.
    PageTable = 0b01,
    /// The descriptor describes an entire 1MB section.
    Section = 0b10,
}

#[bitfield(u32, debug)]
pub struct SectionDescriptor {
    /// Describes the base address of the section.
    #[bits(20..=31, rw)]
    physical_address: u12,
    #[bit(19, rw)]
    non_secure: bool,
    #[bit(17, rw)]
    not_global: bool,
    #[bits(12..=14, rw)]
    tex: u3,
    #[bits([10..=11, 15], rw)]
    access_permissions: Option<AccessPermissions>,
    #[bits(5..=8, rw)]
    domain_id: u4,
    #[bit(4, rw)]
    no_execute: bool,
    #[bit(3, rw)]
    c: bool,
    #[bit(2, rw)]
    b: bool,
}

impl SectionDescriptor {
    /// Return the memory region attributes for the section, assuming SCTLR.TRE is 0.
    pub fn mem_attrs(self) -> Option<MemoryRegionAttributes> {
        let tex = self.tex().value();
        let c = self.c();
        let b = self.b();

        Some(match (tex, c, b) {
            (0b000, false, false) => MemoryRegionAttributes::StronglyOrdered,
            (0b000, false, true) => MemoryRegionAttributes::ShareableDevice,
            (0b000, true, false) => MemoryRegionAttributes::OuterAndInnerWriteThroughNoWriteAlloc,
            (0b000, true, true) => MemoryRegionAttributes::OuterAndInnerWriteBackNoWriteAlloc,
            (0b001, false, false) => MemoryRegionAttributes::OuterAndInnerNonCacheable,
            (0b001, true, true) => MemoryRegionAttributes::OuterAndInnerWriteBackWriteAlloc,
            (0b010, false, false) => MemoryRegionAttributes::NonShareableDevice,
            (tex, c, b) if (tex >> 2 == 1) => {
                let bb = tex & 0b11;
                let aa = ((c as u8) << 1) | b as u8;

                MemoryRegionAttributes::CacheableMemory {
                    inner: CachePolicy::new_with_raw_value(u2::new(aa)),
                    outer: CachePolicy::new_with_raw_value(u2::new(bb)),
                }
            }
            _ => return None,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
#[bitenum(u3, exhaustive = false)]
pub enum AccessPermissions {
    /// All access fails.
    NoAccess = 0b000,
    /// Only PL1 mode can read/write.
    PL1AccessOnly = 0b001,
    /// PL1 mode can read/write, User mode can read.
    PL1AccessUserRead = 0b010,
    /// All modes can read/write.
    AllAccess = 0b011,
    /// Read-only memory. Only PL1 mode can access.
    ReadOnlyPL1AccessOnly = 0b101,
    /// Read-only memory. All modes can access. Deprecated, use [`ReadOnlyAllAccess`] instead.
    ReadOnlyPL1AccessUserRead = 0b110,
    /// Read-only memory. All modes can access.
    ReadOnlyAllAccess = 0b111,
}

impl AccessPermissions {
    /// Returns whether writes are allowed to a section with these restrictions at the given
    /// permission level.
    #[must_use]
    pub const fn test_write(self, pl1: bool) -> bool {
        match self {
            Self::AllAccess => true,
            Self::PL1AccessOnly | Self::PL1AccessUserRead if pl1 => true,
            _ => false,
        }
    }

    /// Returns whether reads are allowed to a section with these restrictions at the given
    /// permission level.
    #[must_use]
    pub const fn test_read(self, pl1: bool) -> bool {
        if self.test_write(pl1) {
            return true;
        }

        match self {
            Self::ReadOnlyAllAccess | Self::ReadOnlyPL1AccessUserRead | Self::PL1AccessUserRead => {
                true
            }
            Self::ReadOnlyPL1AccessOnly if pl1 => true,
            _ => false,
        }
    }
}

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
        Self(<Self as SysRegRead>::read_raw())
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
        unsafe {
            Self::write_raw(self.0);
        }
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

/// Runs a function with "Manager" access on all memory domains. This temporarily bypasses MMU
/// permission checks on the translation table.
///
/// Since a misbehaving program could corrupt device configuration, VEXos protects some lower-level
/// config registers against accidental writes. This function can be used as a marker for
/// "This memory is being accessed intentionally."
#[inline]
pub fn with_manager_domain_access<T>(inner: impl FnOnce() -> T) -> T {
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

#[bitfield(u32, debug)]
pub struct PhysicalAccessRegister {
    #[bit(0, r)]
    fault: bool,
}

impl From<Par> for PhysicalAccessRegister {
    fn from(par: Par) -> Self {
        Self::new_with_raw_value(par.0)
    }
}