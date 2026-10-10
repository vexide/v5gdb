//! Competition status overriding.
//!
//! This module overrides the return values from competition status functions without changing the
//! view of competition mode to VEXos. Therefore, the faked competition status does not override
//! any restrictions imposed by VEXos on I/O during the autonomous and disabled modes. For instance,
//! if the real competition mode is "disabled" but it's overridden to "opcontrol", then user code
//! will see "opcontrol" as the current mode but be unable to write to motor devices.

use core::{
    fmt::{self, Debug, Formatter},
    sync::atomic::{AtomicU32, Ordering},
};

use bitbybit::bitfield;

use crate::sdk::jumptable;

#[bitfield(u32)]
pub struct CompetitionStatus {
    #[bit(0, rw)]
    disabled: bool,
    #[bit(1, rw)]
    autonomous: bool,
    #[bit(2, rw)]
    connected: bool,
    #[bit(3, rw)]
    system: bool,
    #[bit(31, rw)]
    _gdb_override: bool,
}

impl Debug for CompetitionStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mode = if self.disabled() {
            "disabled"
        } else if self.autonomous() {
            "autonomous"
        } else {
            "opcontrol"
        };

        let system = if self.connected() {
            if self.system() {
                "field-control"
            } else {
                "competition-switch"
            }
        } else {
            "disconnected"
        };

        write!(f, "{mode}/{system}")
    }
}

static COMP_STATUS_OVERRIDE: AtomicU32 = AtomicU32::new(0);

pub fn install_override() {
    // SAFETY: The target and the destination have the same signature, preconditions,
    // and postconditions.
    unsafe {
        super::redirect_function(
            vex_sdk::vexCompetitionStatus as *mut (),
            self::read_status as *const (),
        );
    }
}

pub fn set_override(value: Option<CompetitionStatus>) {
    let status = if let Some(status) = value {
        status.with__gdb_override(true)
    } else {
        CompetitionStatus::ZERO
    };

    COMP_STATUS_OVERRIDE.store(status.raw_value(), Ordering::SeqCst);
}

pub fn read_override() -> Option<CompetitionStatus> {
    let status_override =
        CompetitionStatus::new_with_raw_value(COMP_STATUS_OVERRIDE.load(Ordering::SeqCst));

    if !status_override._gdb_override() {
        return None;
    }

    Some(status_override.with__gdb_override(false))
}

pub fn read_real_status() -> CompetitionStatus {
    // vexCompetitionStatus
    unsafe { jumptable!(0x9d8, extern "C" fn() -> CompetitionStatus)() }
}

// ABI identical to vexCompetitionStatus
pub extern "C" fn read_status() -> CompetitionStatus {
    if let Some(status_override) = read_override() {
        return status_override;
    }

    read_real_status()
}
