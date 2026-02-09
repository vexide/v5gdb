#![allow(clippy::missing_safety_doc)]

use core::convert::Infallible;

use gdbstub::{
    arch::Arch,
    stub::MultiThreadStopReason,
    target::{
        Target, TargetError, TargetResult,
        ext::{
            base::{
                BaseOps,
                single_register_access::SingleRegisterAccessOps,
                singlethread::{SingleThreadBase, SingleThreadResumeOps},
            },
            breakpoints::BreakpointsOps,
            host_io::HostIoErrno,
            memory_map::{MemoryMap, MemoryMapOps},
            monitor_cmd::MonitorCmdOps,
        },
    },
};
use zynq7000::devcfg::MmioDevCfg;

use crate::{
    cpu::{ProgramStatus, debug::DebugEventReason},
    exceptions::DebugEventContext,
    gdb_target::{
        arch::{ArmBreakpointKind, ArmRegisters, ArmV7},
        breakpoint::{
            BreakpointError,
            hardware::{HwBreakpointManager, Specificity},
            software::SwBreakpoint,
        },
    },
    sys::{DebuggerSystem, System},
};

pub mod arch;
pub mod breakpoint;
pub mod memory;
pub mod monitor;
pub mod resume;
pub mod single_register_access;
pub mod thread;

/// Debugger state storage.
pub struct V5Target {
    pub exception_ctx: DebugEventContext,
    /// Indicates whether the debugger monitor loop should stop, allowing the program to continue
    /// execution.
    pub resume: bool,
    /// Indicates whether the program is exiting.
    ///
    /// If this goes back to `false`, an exit has been acknowledged by GDB.
    pub exiting: bool,

    /// Indicates whether new software breakpoints should be enabled.
    pub breaks_paused: bool,
    /// The list of breakpoints.
    pub breaks: [Option<SwBreakpoint>; 16],
    pub hw_manager: HwBreakpointManager,
    /// If set, breakpoints are being used to single step. Report any hardware breaks as single
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
            exception_ctx: DebugEventContext::default(),
            resume: false,
            exiting: false,
            breaks_paused: false,
            breaks: [None; _],
            single_step_request: None,
            hw_manager: HwBreakpointManager::setup(devcfg),
        }
    }

    /// Clears the resume flag.
    pub const fn reset_resume(&mut self) {
        self.resume = false;
    }

    /// Marks the debugger as ready to resume.
    pub const fn resume(&mut self) {
        self.resume = true;
    }

    /// Marks the program as pending exit.
    ///
    /// If this is called before entering a debug monitor loop, the debugger will tell GDB that the
    /// program has exited. This will cause the debug monitor to immediately stop, and control will
    /// return to the program to do any final clean-up.
    pub const fn exit_request(&mut self) {
        self.exiting = true;
    }

    /// Create a breakpoint that will stop the debugger after a single instruction has been
    /// executed.
    pub fn setup_step(&mut self) -> Result<(), BreakpointError> {
        if self.single_step_request.is_some() {
            return Ok(());
        }

        let kind = if self.exception_ctx.spsr.thumb() {
            ArmBreakpointKind::Thumb16
        } else {
            ArmBreakpointKind::Arm32
        };

        self.hw_manager.add_breakpoint_at(
            self.exception_ctx.program_counter,
            Specificity::Mismatch,
            kind,
        )?;

        self.single_step_request = Some(SingleStepRequest {
            target_addr: self.exception_ctx.program_counter,
            kind,
        });

        Ok(())
    }

    pub fn get_stop_reason(&self) -> MultiThreadStopReason<u32> {
        if self.exiting {
            return MultiThreadStopReason::Exited(0);
        }

        match self.hw_manager.last_break_reason() {
            Some(DebugEventReason::Breakpoint) => {
                // We don't use MultiThreadStopReason::DoneStep because it doesn't send thread info
                // to GDB (DoneStep is just an alias for SIGTRAP without thread info). HwBreak is
                // essentially the same message but with thread info set.
                MultiThreadStopReason::HwBreak(System::current_thread())
            }
            // GDB allows software breaks to be hardcoded `bkpt` instructions in the program, so
            // there's no need for special handling there.
            _ => MultiThreadStopReason::SwBreak(System::current_thread())
        }
    }
}

impl Target for V5Target {
    type Arch = ArmV7;
    type Error = Infallible;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        if System::MULTITHREADED {
            BaseOps::MultiThread(self)
        } else {
            BaseOps::SingleThread(self)
        }
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }

    fn support_memory_map(&mut self) -> Option<MemoryMapOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadBase for V5Target {
    fn read_registers(&mut self, regs: &mut ArmRegisters) -> TargetResult<(), Self> {
        let ctx = &self.exception_ctx;
        *regs = ArmRegisters {
            r: ctx.registers,
            sp: ctx.stack_pointer,
            lr: ctx.link_register,
            pc: ctx.program_counter,
            d: ctx.vfp_registers,
            fpscr: ctx.fpscr,
            cpsr: ctx.spsr.raw_value(),
        };

        Ok(())
    }

    fn write_registers(&mut self, regs: &<ArmV7 as Arch>::Registers) -> TargetResult<(), Self> {
        let ctx = &mut self.exception_ctx;
        *ctx = DebugEventContext {
            registers: regs.r,
            stack_pointer: regs.sp,
            link_register: regs.lr,
            program_counter: regs.pc,
            spsr: ProgramStatus::new_with_raw_value(regs.cpsr),
            vfp_registers: regs.d,
            fpscr: regs.fpscr,
        };

        Ok(())
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        let bytes_read = memory::read_memory(start_addr, data);
        if bytes_read == 0 {
            return Err(TargetError::Errno(HostIoErrno::EFAULT as u8));
        }

        Ok(bytes_read)
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        if memory::write_memory(start_addr, data) {
            Ok(())
        } else {
            Err(TargetError::Errno(HostIoErrno::EFAULT as u8))
        }
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }
}

impl MemoryMap for V5Target {
    fn memory_map_xml(
        &self,
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let memory_map = include_bytes!("./arch/memory_map.xml");
        if offset > memory_map.len() as u64 {
            return Ok(0);
        }
        let slice = &memory_map[offset as usize..];
        let count = slice.len().min(length);
        buf[..count].copy_from_slice(&slice[..count]);
        Ok(count)
    }
}
