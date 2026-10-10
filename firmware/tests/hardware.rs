use arbitrary_int::u4;
use v5gdb::gdb_target::{
    arch::ArmBreakpointKind,
    breakpoint::{
        BreakpointError,
        hardware::{HwBreakpointManager, Specificity},
    },
};
use vexide::prelude::*;

mod common;

common::test_harness!(test);

fn test(_p: Peripherals) -> anyhow::Result<()> {
    let mut zp = zynq7000::Peripherals::take().unwrap();
    let mut manager = HwBreakpointManager::setup(&mut zp.devcfg);
    manager.set_locked(false);

    // -- Test normal breaks
    manager.add_breakpoint_at(0x0380_0000, Specificity::Match, ArmBreakpointKind::Arm32)?;

    let mmio = unsafe { manager.mmio() };

    let bp0 = mmio.read_breakpoint_value(0)?;
    assert_eq!(bp0, 0x0380_0000);

    let bp0_ctrl = mmio.read_breakpoint_ctrl(0)?;
    assert!(bp0_ctrl.enabled());
    assert_eq!(bp0_ctrl.byte_address_select(), u4::new(0b1111));

    manager.reset();

    // Test reset
    let mmio = unsafe { manager.mmio() };
    let bp0_ctrl = mmio.read_breakpoint_ctrl(0)?;
    assert!(!bp0_ctrl.enabled());

    // -- Breaks with a size of 2 should select only part of the 4-byte breakpoint.
    manager.add_breakpoint_at(0x0380_0000, Specificity::Match, ArmBreakpointKind::Thumb16)?;

    let mmio = unsafe { manager.mmio() };
    let bp0_ctrl = mmio.read_breakpoint_ctrl(0)?;
    // Only trigger on the first 2 bytes.
    assert_eq!(bp0_ctrl.byte_address_select(), u4::new(0b0011));

    manager.reset();
    manager.add_breakpoint_at(0x0380_0002, Specificity::Match, ArmBreakpointKind::Thumb16)?;

    let mmio = unsafe { manager.mmio() };
    let bp0_ctrl = mmio.read_breakpoint_ctrl(0)?;
    // Only trigger on the last 2 bytes.
    assert_eq!(bp0_ctrl.byte_address_select(), u4::new(0b1100));

    // -- Test duplicate
    let res =
        manager.add_breakpoint_at(0x0380_0002, Specificity::Match, ArmBreakpointKind::Thumb16);
    assert_eq!(res, Err(BreakpointError::AlreadyExists));

    // -- Test misalignment
    manager.reset();

    let res = manager.add_breakpoint_at(0xfff2, Specificity::Match, ArmBreakpointKind::Arm32);
    assert_eq!(res, Err(BreakpointError::NotAlignedCorrectly));

    manager.add_breakpoint_at(0xfff2, Specificity::Match, ArmBreakpointKind::Thumb16)?;

    let res = manager.add_breakpoint_at(0xfff1, Specificity::Match, ArmBreakpointKind::Thumb16);
    assert_eq!(res, Err(BreakpointError::NotAlignedCorrectly));

    // -- Test too many
    manager.reset();

    let num_breaks = manager.capabilities().num_breakpoints as u32;

    for i in 0..num_breaks {
        manager.add_breakpoint_at(i * 4, Specificity::Match, ArmBreakpointKind::Arm32)?;
    }

    // (One too many.)
    let res =
        manager.add_breakpoint_at(num_breaks * 4, Specificity::Match, ArmBreakpointKind::Arm32);
    assert_eq!(res, Err(BreakpointError::NoSpace));

    // -- Cleanup
    manager.reset();
    manager.set_locked(true);

    Ok(())
}
