use core::fmt::{self, Debug, Formatter};

use aarch32_cpu::cache::clean_and_invalidate_data_cache_line_to_poc;
use arbitrary_int::*;
use gdbstub::target::{TargetResult, ext::breakpoints::HwBreakpoint};
use zynq7000::devcfg;

use crate::{
    cpu::{
        debug::{
            BreakpointControl, BreakpointType, DEBUG_UNLOCK_MAGIC, DebugEventReason, DebugID,
            DebugLogic, DebugROMAddress, DebugSelfAddressOffset, DebugValid, MmioDebugLogic,
            PrivilegeModeFilter, SecureDebugEnable, SecurityFilter, WatchpointControl,
        },
        vmsa::with_manager_domain_access,
    },
    gdb_target::{V5Target, arch::ArmBreakpointKind, breakpoint::BreakpointError},
    logging,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareCapabilities {
    pub num_breakpoints: u8,
    pub num_watchpoints: u8,
}

/// The desired configuration of one hardware breakpoint slot.
#[derive(Debug, Clone, Copy)]
struct HwSlot {
    value: u32,
    ctrl: BreakpointControl,
}

pub struct HwBreakpointManager {
    capabilities: HardwareCapabilities,
    mmio: MmioDebugLogic<'static>,
    /// The list of hardware breakpoints currently configured.
    ///
    /// This is the source of truth for which breakpoints exist. The debug registers mirror it
    /// unless the manager is paused, in which case every breakpoint is disabled.
    slots: [Option<HwSlot>; 16],
    /// Whether breakpoints are currently temporarily disabled in hardware.
    paused: bool,
}

impl HwBreakpointManager {
    /// Sets up hardware debugging.
    ///
    /// The returned breakpoint manager is software locked by default.
    ///
    /// # Panics
    ///
    /// A panic is triggered if:
    ///
    /// - CPU debug features are hardware locked by the board.
    /// - The device has no MMIO interface for debug registers.
    pub fn setup(devcfg: &mut devcfg::MmioRegisters<'_>) -> Self {
        // Enable access to the board's debug hardware. The devcfg registers are protected against
        // accidental writes, so we have to do extra work to access them or else we get a data abort
        // with "Permission fault (MMU)".
        let enabled = critical_section::with(|_| {
            with_manager_domain_access(|| {
                clean_and_invalidate_data_cache_line_to_poc(devcfg.pointer_to_control() as u32);

                // Code that runs before us might have disabled writes to the debug logic, so
                // fail early if it's locked OFF.
                let lock = devcfg.read_lock();
                if lock.debug() {
                    let ctrl = devcfg.read_control();
                    return ctrl.invasive_debug_enable() && ctrl.secure_invasive_debug_enable();
                }

                // Enable the CPU's invasive debug features.
                devcfg.modify_control(|ctrl| {
                    ctrl.with_invasive_debug_enable(true)
                        .with_secure_invasive_debug_enable(true)
                });

                true
            })
        });

        assert!(
            enabled,
            "The operating system has disabled hardware debugging."
        );

        // Enable debugging in the Secure PL0 processor mode. (Secure PL1 is controlled by the
        // Zynq's devcfg.CTRL.SPIDEN, we already enabled it.)
        let secure_debug = SecureDebugEnable::read();
        secure_debug.with_secure_user_invasive_debug(true).write();

        // Look up where we will access debug MMIO from.

        let rom_base = DebugROMAddress::read();
        let self_address_offset = DebugSelfAddressOffset::read();
        assert!(
            rom_base.valid() == Ok(DebugValid::Valid)
                && self_address_offset.valid() == Ok(DebugValid::Valid),
            "This device has no debug logic MMIO"
        );

        let mmio_base = rom_base
            .value()
            .wrapping_add_signed(self_address_offset.value());

        let debug_id = DebugID::read();
        let num_breakpoints = debug_id.brps().value() + 1;
        let num_watchpoints = debug_id.wrps().value() + 1;

        let mut manager = Self {
            capabilities: HardwareCapabilities {
                num_breakpoints,
                num_watchpoints,
            },
            mmio: unsafe { DebugLogic::new_mmio_at(mmio_base) },
            slots: [None; _],
            paused: false,
        };

        manager.set_locked(false);
        manager.reset();
        manager.set_locked(true);
        manager
    }

    /// Disable all existing breakpoints and enable Monitor (debug exception) hardware debug mode.
    pub fn reset(&mut self) {
        // This doesn't have any meaning, it's just a value that clearly isn't random or a real
        // address.
        const RESET_SENTINEL: u32 = 0xf0f0f0f0;

        assert!(!self.locked(), "Debug registers must be unlocked");

        // Note: before enabling halt or monitor debug mode for the first time, all breakpoints and
        // watchpoints need to be explicitly set as either enabled or disabled.

        for idx in 0..self.capabilities.num_breakpoints {
            self.mmio
                .write_breakpoint_ctrl(idx.into(), BreakpointControl::DISABLED)
                .unwrap();
            self.mmio
                .write_breakpoint_value(idx.into(), RESET_SENTINEL)
                .unwrap();
        }
        self.slots = [None; 16];

        for idx in 0..self.capabilities.num_watchpoints {
            self.mmio
                .write_watchpoint_ctrl(idx.into(), WatchpointControl::DISABLED)
                .unwrap();
            self.mmio
                .write_watchpoint_value(idx.into(), RESET_SENTINEL)
                .unwrap();
        }

        aarch32_cpu::asm::dsb();

        // Route breakpoint/watchpoint debug events to debug exceptions. This allows us to catch
        // them at runtime as prefetch/data aborts instead of halting the processor.
        // (see Table C3-1 Processor behavior on debug events)
        self.mmio.modify_status_control_ext(|debug_ctrl| {
            debug_ctrl
                .with_halting_debug_mode(false)
                .with_monitor_debug_mode(true)
        });

        aarch32_cpu::asm::dsb();
        aarch32_cpu::asm::isb();
    }

    #[must_use]
    pub const fn capabilities(&self) -> HardwareCapabilities {
        self.capabilities
    }

    /// Registers and activates a hardware breakpoint matching the given address.
    ///
    /// # Errors
    ///
    /// An error is returned if there are no more hardware breakpoints available, or if the
    /// breakpoint already exists.
    pub fn add_breakpoint_at(
        &mut self,
        addr: u32,
        specificity: Specificity,
        kind: ArmBreakpointKind,
    ) -> Result<(), BreakpointError> {
        assert!(!self.locked(), "Debug registers must be unlocked");

        let (new_word, new_bas) = split_addr(addr, kind)?;
        let new_type: BreakpointType = specificity.into();

        let num_breakpoints = self.capabilities.num_breakpoints as usize;
        let mut free_idx = None;

        for (idx, slot) in self.slots[..num_breakpoints].iter().enumerate() {
            match slot {
                // We intentionally don't break here to make sure we've checked all slots for
                // duplicates.
                None if free_idx.is_none() => free_idx = Some(idx),
                Some(slot)
                    if slot.ctrl.breakpoint_type() == Ok(new_type)
                        && slot.value == new_word
                        && slot.ctrl.byte_address_select() == new_bas =>
                {
                    return Err(BreakpointError::AlreadyExists);
                }
                _ => {}
            }
        }

        let Some(bkpt_index) = free_idx else {
            return Err(BreakpointError::NoSpace);
        };

        // We set the breakpoint value to the 4-byte word containing the address because breakpoints
        // look at regions 4 bytes large and must be aligned as such.
        let ctrl = BreakpointControl::DISABLED
            .with_enabled(true)
            .with_byte_address_select(new_bas)
            // No mask, match exact address
            .with_address_range_mask(u5::new(0b00000))
            // No linked Context ID breakpoint
            .with_linked_breakpoint_index(u4::new(0))
            .with_breakpoint_type(new_type)
            // Don't trigger inside abort mode, and "step over" IRQs
            .with_privileged_mode_ctrl(PrivilegeModeFilter::UserSystemSupervisorOnly)
            .with_security_state_ctrl(SecurityFilter::All);

        self.slots[bkpt_index] = Some(HwSlot {
            value: new_word,
            ctrl,
        });
        self.sync_slot(bkpt_index);

        aarch32_cpu::asm::dsb();
        aarch32_cpu::asm::isb();

        Ok(())
    }

    /// Removes all breakpoints at the given address with the given kind and type.
    ///
    /// Returns whether any changes were made.
    pub fn remove_breakpoint_at(
        &mut self,
        addr: u32,
        specificity: Specificity,
        kind: ArmBreakpointKind,
    ) -> bool {
        assert!(!self.locked(), "Debug registers must be unlocked");

        let Ok((search_word, byte_address_select)) = split_addr(addr, kind) else {
            return false;
        };
        let search_type = BreakpointType::from(specificity);

        let mut anything_removed = false;
        for bkpt_index in 0..self.capabilities.num_breakpoints as usize {
            let Some(slot) = self.slots[bkpt_index] else {
                continue;
            };

            if slot.ctrl.breakpoint_type() != Ok(search_type)
                || slot.ctrl.byte_address_select() != byte_address_select
                || slot.value != search_word
            {
                continue;
            }

            self.slots[bkpt_index] = None;
            self.sync_slot(bkpt_index);
            anything_removed = true;
        }

        aarch32_cpu::asm::dsb();
        aarch32_cpu::asm::isb();

        anything_removed
    }

    /// Returns whether breakpoints are being kept out of the debug registers.
    #[must_use]
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Pauses or resumes all hardware breakpoints.
    ///
    /// While paused, every breakpoint is disabled in the debug registers, so none of them can
    /// trigger no matter which CPU mode is running. Breakpoints can still be added and removed, and
    /// they take effect when the breakpoint manager is unpaused.
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;

        for idx in 0..self.capabilities.num_breakpoints as usize {
            self.sync_slot(idx);
        }

        aarch32_cpu::asm::dsb();
        aarch32_cpu::asm::isb();
    }

    /// Writes the desired state of a breakpoint slot to the debug registers.
    ///
    /// The caller must issue the barriers needed to make the change take effect.
    fn sync_slot(&mut self, idx: usize) {
        assert!(!self.locked(), "Debug registers must be unlocked");

        match self.slots[idx] {
            Some(slot) if !self.paused => {
                // Disable the slot first to avoid hitting a half-written breakpoint.
                self.mmio
                    .write_breakpoint_ctrl(idx, BreakpointControl::DISABLED)
                    .unwrap();
                self.mmio.write_breakpoint_value(idx, slot.value).unwrap();
                self.mmio.write_breakpoint_ctrl(idx, slot.ctrl).unwrap();
            }
            _ => {
                self.mmio
                    .write_breakpoint_ctrl(idx, BreakpointControl::DISABLED)
                    .unwrap();
            }
        }
    }

    /// Returns the number of hardware breakpoints that aren't being used.
    #[must_use]
    pub fn breakpoints_available(&self) -> u8 {
        let breaks_used = self.slots.iter().filter(|slot| slot.is_some()).count() as u8;
        let total_breaks = self.capabilities.num_breakpoints;

        total_breaks - breaks_used
    }

    /// Returns the reason why the most recent debug event was triggered.
    #[must_use]
    pub fn last_break_reason(&self) -> Option<DebugEventReason> {
        let status = self.mmio.read_status_control_ext();
        status.method_of_entry().ok()
    }

    /// Indicates whether changes to hardware breakpoints (via MMIO) are disabled.
    ///
    /// This breakpoint manager abstraction will panic if MMIO is locked and a caller attempts to
    /// make changes.
    pub fn locked(&self) -> bool {
        self.mmio.read_lock_status().software_lock_status()
    }

    /// Sets whether changes to hardware breakpoints (via MMIO) are disabled.
    pub fn set_locked(&mut self, locked: bool) {
        if locked {
            self.mmio.write_lock_access(0);
        } else {
            self.mmio.write_lock_access(DEBUG_UNLOCK_MAGIC);
        }
        debug_assert!(self.locked() == locked);
    }

    pub unsafe fn mmio(&self) -> &MmioDebugLogic<'_> {
        &self.mmio
    }
}

/// Controls a breakpoint or watchpoint's association with its address(es).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Specificity {
    /// The breakpoint/watchpoint is triggered when the specified addresses are accessed.
    Match,
    /// The breakpoint/watchpoint is triggered when an address other than the ones specified
    /// are accessed.
    Mismatch,
}

impl From<Specificity> for BreakpointType {
    fn from(value: Specificity) -> Self {
        match value {
            Specificity::Match => BreakpointType::UnlinkedInstrAddressMatch,
            Specificity::Mismatch => BreakpointType::UnlinkedInstrAddressMismatch,
        }
    }
}

impl Debug for HwBreakpointManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut bkpt_values = [0; 16];
        let mut bkpt_ctrls = [BreakpointControl::ZERO; 16];

        for i in 0..self.capabilities.num_breakpoints.min(16) {
            bkpt_values[i as usize] = self.mmio.read_breakpoint_value(i as usize).unwrap();
            bkpt_ctrls[i as usize] = self.mmio.read_breakpoint_ctrl(i as usize).unwrap();
        }

        f.debug_struct("HwBreakpointManager")
            .field("locked", &self.locked())
            .field("capabilities", &self.capabilities)
            .field("paused", &self.paused)
            .field(
                "slots",
                &&self.slots[..self.capabilities.num_breakpoints as usize],
            )
            .field("mmio_ptr", &unsafe { self.mmio.ptr() })
            .field(
                "bkpt_values",
                &&bkpt_values[..self.capabilities.num_breakpoints as usize],
            )
            .field(
                "bkpt_ctrls",
                &&bkpt_ctrls[..self.capabilities.num_breakpoints as usize],
            )
            .finish_non_exhaustive()
    }
}

/// Splits an address into the word containing it and the byte-address-select that would match
/// the instruction's offset into the word.
fn split_addr(addr: u32, kind: ArmBreakpointKind) -> Result<(u32, u4), BreakpointError> {
    let word = addr & !0b11;

    // Specify which addresses inside the 4-byte breakpoint to match. Multi-byte instructions
    // are considered to inhabit all of their addresses at once.
    let byte_address_select = match kind {
        // The instruction spans 4 bytes, so the breakpoint needs to match over its the entire
        // 4-byte value: [0-3].
        ArmBreakpointKind::Arm32 => {
            if !addr.is_multiple_of(4) {
                return Err(BreakpointError::NotAlignedCorrectly);
            }
            u4::new(0b1111)
        }
        // 16-bit Thumb address - match either addresses ending in [0-1] or [2-3], depending on
        // which side of the word it's aligned to.
        // (Although 4-byte Thumb instructions are a thing, we can treat them the same as 2-byte
        // ones. See <Table C3-2> Effect of byte address selection on Breakpoint generation.)
        ArmBreakpointKind::Thumb16 | ArmBreakpointKind::Thumb32 => {
            if !addr.is_multiple_of(2) {
                return Err(BreakpointError::NotAlignedCorrectly);
            }
            if addr.is_multiple_of(4) {
                u4::new(0b0011)
            } else {
                u4::new(0b1100)
            }
        }
    };

    Ok((word, byte_address_select))
}

impl HwBreakpoint for V5Target {
    fn add_hw_breakpoint(
        &mut self,
        addr: u32,
        kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        if self.hw_manager.breakpoints_available() <= 1 {
            // One hardware breakpoint should be saved for single stepping.
            logging::warn!("Can't add hardware breakpoint at {addr:#010x}: all slots are in use");
            return Ok(false);
        }

        match self
            .hw_manager
            .add_breakpoint_at(addr, Specificity::Match, kind)
        {
            Ok(()) => {
                logging::info!("Added hardware breakpoint at {addr:#010x} ({kind:?})");
                Ok(true)
            }
            Err(err) => {
                logging::warn!("Can't add hardware breakpoint at {addr:#010x}: {err}");
                Ok(false)
            }
        }
    }

    fn remove_hw_breakpoint(
        &mut self,
        addr: u32,
        kind: ArmBreakpointKind,
    ) -> TargetResult<bool, Self> {
        // LLDB adds Thumb breakpoints with kind 2 but removes them with kind 4, so if nothing
        // matches the requested kind, try the others too.
        let did_remove = [kind, ArmBreakpointKind::Thumb16, ArmBreakpointKind::Arm32]
            .into_iter()
            .any(|kind| {
                self.hw_manager
                    .remove_breakpoint_at(addr, Specificity::Match, kind)
            });
        if did_remove {
            logging::info!("Removed hardware breakpoint at {addr:#010x}");
        } else {
            logging::debug!("No hardware breakpoint to remove at {addr:#010x}");
        }

        Ok(did_remove)
    }
}
