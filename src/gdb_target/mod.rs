#![allow(clippy::missing_safety_doc)]

use std::convert::Infallible;

use gdbstub::{
    arch::Arch,
    target::{
        Target, TargetError, TargetResult,
        ext::{
            base::{
                BaseOps,
                single_register_access::SingleRegisterAccessOps,
                singlethread::{SingleThreadBase, SingleThreadResumeOps},
            },
            breakpoints::{BreakpointsOps, SwBreakpoint},
            monitor_cmd::MonitorCmdOps,
        },
    },
};
use zynq7000::devcfg::{DevCfg, MmioDevCfg};

use crate::{
    cache,
    exception::{DebugEventContext, ProgramStatus},
    gdb_target::{
        arch::{
            ArmBreakpointKind, ArmRegisters, ArmV7, hw::{HwBreakpointManager, Specificity}
        },
        breakpoint::{BreakpointError, SWBreakpoint},
    },
    instruction::Instruction,
};

pub mod arch;
pub mod breakpoint;
pub mod monitor;
pub mod resume;
pub mod single_register_access;

/// Debugger state storage.
pub struct V5Target {
    pub exception_ctx: Option<DebugEventContext>,
    /// Indicates whether the debugger monitor loop should stop, allowing the program to continue
    /// execution.
    pub resume: bool,

    /// The list of breakpoints.
    pub breaks: [Option<SWBreakpoint>; 16],
    pub hw_manager: HwBreakpointManager,
    /// If set, the hardware breakpoint system is being used to register a callback to re-enable a
    /// software breakpoint after continuing, so when a break occurs we should only run fixups
    /// and continue.
    pub breakpoint_pending_reenable: Option<Breakpoint>,
    /// If set, nreakpoints are being used to single step. Report any hardware breaks as single
    /// steps instead of normal breakpoints.
    pub single_step_request: Option<SingleStepRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SingleStepRequest {
    /// The address of the instruction that is being stepped over.
    pub target_addr: u32,
    pub kind: ArmBreakpointKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breakpoint {
    pub addr: u32,
    pub is_thumb: bool,
    pub is_hardware: bool,
}

impl V5Target {
    #[must_use]
    pub fn new(devcfg: &mut MmioDevCfg<'_>) -> Self {
        Self {
            exception_ctx: None,
            resume: false,
            breaks: [None; _],
            breakpoint_pending_reenable: None,
            single_step_request: None,
            hw_manager: HwBreakpointManager::setup(devcfg),
        }
    }

    /// Temporarily disables the current breakpoint so that returning from it will continue
    /// execution rather than re-triggering it immediately.
    ///
    /// This will also configure the processor to single-step one instruction, then re-enable the
    /// specified breakpoint (if the specified breakpoint existed in the first place).
    pub fn prepare_for_continue(&mut self, this_bkpt: Breakpoint) {

        // Disabling the current breakpoint allows us to continue execution without immediately
        // triggering it again.
        let changes_made = if this_bkpt.is_hardware {
            // FIXME: Thumb32 breakpoints decay to Thumb16 after they are used once.
            // We need some way to look up the address of a breakpoint and see what the kind is.
            let kind = if this_bkpt.is_thumb {
                ArmBreakpointKind::Thumb16
            } else {
                ArmBreakpointKind::Arm32
            };

            self.hw_manager
                .remove_breakpoint_at(this_bkpt.addr, Specificity::Match, kind)
        } else {
            self.remove_sw_breakpoint(this_bkpt.addr)
        };

        let exception_ctx = self
            .exception_ctx
            .as_ref()
            .expect("The debugger target has no exception context set");

        if !changes_made {
            // The breakpoint doesn't exist (anymore), so don't try to re-enable it.
            return;
        }

        // Create an internal single-step breakpoint that, when activated, will re-enable this
        // breakpoint and promptly continue program execution. This is used to support persistent
        // breakpoints, since returning from a breakpoint requires you to temporarily disable it
        // (otherwise it would immediately trigger again).

        let kind = if exception_ctx.spsr.is_thumb() {
            ArmBreakpointKind::Thumb16
        } else {
            ArmBreakpointKind::Arm32
        };

        self.hw_manager
            .add_breakpoint_at(exception_ctx.program_counter, Specificity::Mismatch, kind)
            .expect("Failed to make fixup breakpoint");
        self.breakpoint_pending_reenable = Some(this_bkpt);
    }

    /// Applies any pending breakpoint fixup operation.
    ///
    /// Returns whether any changes were made.
    pub fn apply_fixup(&mut self) -> bool {
        let Some(bkpt) = self.breakpoint_pending_reenable.take()
        else {
            return false;
        };

        if bkpt.is_hardware {
            let kind = if bkpt.is_thumb {
                ArmBreakpointKind::Thumb16
            } else {
                ArmBreakpointKind::Arm32
            };

            self.hw_manager
                .add_breakpoint_at(bkpt.addr, Specificity::Match, kind)
                .expect("Failed to re-enable breakpoint after continuing");
        } else {
            unsafe {
                self.register_sw_breakpoint(
                    bkpt.addr,
                    bkpt.is_thumb,
                )
                .expect("Failed to re-enable breakpoint after continuing");
            }
        }

        true
    }

    /// Clears the resume flag.
    pub const fn reset_resume(&mut self) {
        self.resume = false;
    }

    /// Marks the debugger as ready to resume.
    pub const fn resume(&mut self) {
        self.resume = true;
    }

    /// Prepare the debugger to resume, step one instruction, then stop again.
    pub fn setup_step(&mut self) -> Result<(), BreakpointError> {
        let exception_ctx = self
            .exception_ctx
            .as_ref()
            .expect("The debugger target has no exception context set");

        let kind = if exception_ctx.spsr.is_thumb() {
            ArmBreakpointKind::Thumb16
        } else {
            ArmBreakpointKind::Arm32
        };

        self.hw_manager.add_breakpoint_at(
            exception_ctx.program_counter,
            Specificity::Mismatch,
            kind,
        )?;

        self.resume = true;
        self.single_step_request = Some(SingleStepRequest {
            target_addr: exception_ctx.program_counter,
            kind,
        });

        Ok(())
    }
}

impl Target for V5Target {
    type Arch = ArmV7;
    type Error = Infallible;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadBase for V5Target {
    fn read_registers(&mut self, regs: &mut ArmRegisters) -> TargetResult<(), Self> {
        if let Some(ctx) = &mut self.exception_ctx {
            *regs = ArmRegisters {
                r: ctx.registers,
                sp: ctx.stack_pointer,
                lr: ctx.link_register,
                pc: ctx.program_counter,
                d: ctx.vfp_registers,
                fpscr: ctx.fpscr,
                cpsr: ctx.spsr.to_raw(),
            };
        } else {
            return Err(TargetError::NonFatal);
        }

        Ok(())
    }

    fn write_registers(&mut self, regs: &<ArmV7 as Arch>::Registers) -> TargetResult<(), Self> {
        if let Some(ctx) = &mut self.exception_ctx {
            *ctx = DebugEventContext {
                registers: regs.r,
                stack_pointer: regs.sp,
                link_register: regs.lr,
                program_counter: regs.pc,
                spsr: ProgramStatus(regs.cpsr),
                vfp_registers: regs.d,
                fpscr: regs.fpscr,
            };
        } else {
            return Err(TargetError::NonFatal);
        }

        Ok(())
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        // TODO: check MMU table to ensure these pages are readable.
        unsafe {
            core::ptr::copy(start_addr as *const u8, data.as_mut_ptr(), data.len());
        }

        Ok(data.len())
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        unsafe {
            core::ptr::copy(data.as_ptr(), start_addr as *mut u8, data.len());
        }

        Ok(())
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }
}
