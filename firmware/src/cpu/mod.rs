use arbitrary_int::*;
use bitbybit::{bitenum, bitfield};

#[cfg(target_arch = "arm")]
pub mod cache;
#[cfg(target_arch = "arm")]
pub mod debug;
#[cfg(target_arch = "arm")]
pub mod exception;
#[cfg(target_arch = "arm")]
pub mod instruction;
#[cfg(target_arch = "arm")]
pub mod vmsa;

/// The status of an ARMv7-A CPU.
#[bitfield(u32, debug, default = 0)]
#[derive(PartialEq, Eq)]
pub struct ProgramStatus {
    #[bit(31, rw)]
    negative: bool,
    #[bit(30, rw)]
    zero: bool,
    #[bit(29, rw)]
    carry: bool,
    #[bit(28, rw)]
    overflow: bool,
    #[bit(27, rw)]
    cumulative_sat: bool,
    #[bits(25..=26, rw)]
    if_then_1_0: u2,
    #[bit(24, rw)]
    jazelle: bool,
    #[bits(16..=19, rw)]
    gt_eq_flags: u4,
    #[bits(10..=15, rw)]
    if_then_7_2: u6,
    #[bit(9, rw)]
    big_endian: bool,
    #[bit(8, rw)]
    async_abort_disabled: bool,
    #[bit(7, rw)]
    irq_disabled: bool,
    #[bit(6, rw)]
    fiq_disabled: bool,
    #[bit(5, rw)]
    thumb: bool,
    #[bits(0..=4, rw)]
    mode: Option<CpuMode>,
}

impl ProgramStatus {
    pub const fn raw_value_mut(&mut self) -> &mut u32 {
        &mut self.raw_value
    }
}

#[bitenum(u5, exhaustive = false)]
#[derive(Debug, PartialEq, Eq)]
pub enum CpuMode {
    Usr = 0b10000,
    Fiq = 0b10001,
    Irq = 0b10010,
    Svc = 0b10011,
    Mon = 0b10110,
    Abt = 0b10111,
    Und = 0b11011,
    Sys = 0b11111,
}
