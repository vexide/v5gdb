#![allow(clippy::missing_safety_doc)]

use core::convert::Infallible;

use gdbstub::{
    common::Signal,
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
use spin::Once;
use zynq7000::devcfg;

use crate::{
    cpu::debug::DebugEventReason,
    exceptions::DebugEventContext,
    gdb_target::{
        arch::{ArmBreakpointKind, ArmV7},
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

/// A fixed address that never matches real code.
const ASYNC_HALT_SENTINEL: u32 = 0xFFFF_FFFC;

/// Why execution stopped at the current PC.
///
/// Helps determine what stop reason to send to GDB and how to set up the debug console.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum StopReason {
    /// A hardware breakpoint was triggered, or a single-step was completed.
    HardwareBreak,
    /// A `bkpt` instruction managed by v5gdb was triggered.
    TrackedSoftwareBreak { id: usize },
    /// A `bkpt` instruction hard-coded in the program source code was triggered.
    UntrackedSoftwareBreak,
    /// The host asynchronously requested a halt while the program was running.
    ///
    /// This can happen if the user presses Ctrl-C in GDB to pause the program.
    Interrupt,
    /// The program stopped for some other reason.
    ///
    /// It's probably only possible for this to be a watchpoint since most other debug events (like
    /// halt request and OS Unlock) are only ever halting.
    Other,
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

/// Tracks the lifecycle of the debug monitor.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MonitorStatus {
    /// The program is paused and the monitor is running.
    Active,
    /// The program is paused immediately prior to exiting and the monitor intends to disconnect
    /// the GDB client.
    Exiting,
    /// The program will be resumed shortly.
    ResumingProgram,
}

/// Receives callbacks from `gdbstub` and keeps track of state required to drive the debugger.
pub struct V5Target {
    /// The working copy of the CPU state, initialized by capturing the state of the program
    /// when it hit the most recent breakpoint.
    ///
    /// Note that updating these fields will cause the debugger to apply those changes to the
    /// program state upon resume.
    exception_ctx: DebugEventContext,
    pub monitor_status: MonitorStatus,

    /// Why the most recent debugger entry occurred.
    stop_reason: StopReason,

    /// Indicates whether software breakpoints are temporarily disabled.
    ///
    /// If true, enabling new software breakpoints will be delayed until breakpoints are unpaused.
    breaks_paused: bool,
    /// The list of software breakpoints.
    breaks: [Option<SwBreakpoint>; 16],

    hw_manager: HwBreakpointManager,
    /// Tracks whether the hardware breakpoint manager should be re-locked when exiting a
    /// breakpoint.
    original_hw_lock_state: bool,

    /// If set, breakpoints are being used to single step. Report any hardware breaks as single
    /// steps instead of normal breakpoints.
    single_step_request: Option<SingleStepRequest>,

    /// If set, the next instruction executed in user code will trigger entry into the debug
    /// monitor and be reported to GDB as SIGINT.
    interrupt_pending: bool,
}

impl V5Target {
    #[must_use]
    pub fn new(devcfg: &mut devcfg::MmioRegisters<'_>) -> Self {
        Self {
            exception_ctx: DebugEventContext::default(),
            monitor_status: MonitorStatus::ResumingProgram,
            stop_reason: StopReason::Other,
            breaks_paused: false,
            breaks: [None; _],
            single_step_request: None,
            interrupt_pending: false,
            original_hw_lock_state: false,
            hw_manager: HwBreakpointManager::setup(devcfg),
        }
    }

    /// Activate the debugger after a breakpoint is triggered.
    pub fn enter_breakpoint(&mut self, ctx: &mut DebugEventContext) -> StopReason {
        // Pause software breakpoints before allowing unpredictable control flow (by interrupts).
        self.set_breakpoints_ignored(true);

        // We re-enable interrupts after the abort (so that UART works) but prevent the RTOS from
        // preempting us. When the debugger is active, the system should appear paused.

        // If we're handling a single-step completion, the scheduler is already disabled from when
        // the step was initiated (previous debug session), so there's no need to do that again.
        if self.single_step_request.is_none() {
            System::suspend_preemption();
        }
        unsafe {
            aarch32_cpu::interrupt::enable();
        }

        log::debug!("Entered debug event handler");
        static BKPT_LOG: Once = Once::new();
        BKPT_LOG.call_once(|| {
            log::error!("**** v5gdb: BREAKPOINT TRIGGERED ****");
            log::error!("Your program has been paused. Please connect a debugger.")
        });

        self.original_hw_lock_state = self.hw_manager.locked();
        self.hw_manager.set_locked(false);

        self.exception_ctx = ctx.clone();
        self.monitor_status = MonitorStatus::Active;
        self.classify_stop();
        self.finalize_single_step();
        self.finalize_interrupt();
        self.fixup_manual_bkpt();

        self.stop_reason
    }

    /// Deactivate the debugger and apply pending changes.
    pub fn leave_breakpoint(&mut self, ctx: &mut DebugEventContext) -> bool {
        // Write back any modifications back so the debug event handler can apply them.
        *ctx = self.exception_ctx.clone();

        log::debug!("Exiting debug event handler");

        // Single steps run with the scheduler off so that we are guaranteed to step the current
        // task, not a different one. - Side note: If PROS implemented ARM's context id register, we
        // could just filter the single step breakpoint by task id and there would be no need for
        // this.
        let should_unpause_scheduler = self.single_step_request.is_none();

        self.hw_manager.set_locked(self.original_hw_lock_state);
        self.set_breakpoints_ignored(false);

        should_unpause_scheduler
    }

    /// Create a breakpoint that will stop the debugger after a single instruction has been
    /// executed.
    pub fn request_single_step(&mut self) -> Result<(), BreakpointError> {
        if self.single_step_request.is_some() {
            return Ok(());
        }

        log::debug!("Preparing single step operation");

        let kind = if self.exception_ctx.cpsr.thumb() {
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

    /// Arm an asynchronous halt in response to an interrupt from GDB.
    ///
    /// Calling this installs a hardware breakpoint which fires on any instructions on which the CPU
    /// is running in user/system/supervisor mode, causing the program to enter into the debug
    /// monitor as soon as any in-progress exceptions have finished. Once stopped, the event is
    /// reported to GDB as SIGINT.
    ///
    /// This may be called from an interrupt context.
    ///
    /// This operation can be considered a stronger variant of [`Self::request_single_step`] which
    /// pauses the program on the *current* instruction rather than the next one. It can be used to
    /// to break out of situations that a single step would not be able to handle, such as a
    /// self-branch (`b .`) in which the program becomes stuck and never advances to the next
    /// instruction.
    pub fn request_interrupt(&mut self) -> Result<(), BreakpointError> {
        // No need to register another interrupt breakpoint if we already have one ready.
        if self.interrupt_pending {
            return Ok(());
        }

        let was_locked = self.hw_manager.locked();
        self.hw_manager.set_locked(false);

        let result = self.hw_manager.add_breakpoint_at(
            ASYNC_HALT_SENTINEL,
            Specificity::Mismatch,
            ArmBreakpointKind::Arm32,
        );

        self.hw_manager.set_locked(was_locked);

        result?;
        self.interrupt_pending = true;
        Ok(())
    }

    /// Remove a breakpoint installed by [`Self::request_interrupt`], if any.
    fn finalize_interrupt(&mut self) {
        if self.interrupt_pending {
            self.hw_manager.remove_breakpoint_at(
                ASYNC_HALT_SENTINEL,
                Specificity::Mismatch,
                ArmBreakpointKind::Arm32,
            );
            self.interrupt_pending = false;
        }
    }

    /// Mark the pending single step request (if any) as completed and clean up its state.
    fn finalize_single_step(&mut self) {
        // If we previously wanted to single step, we can permanently remove the breakpoint that
        // supported that now. The single step request is then cleared since we've finished all
        // required cleanup.
        if let Some(single_step) = self.single_step_request.take() {
            self.hw_manager.remove_breakpoint_at(
                single_step.target_addr,
                Specificity::Mismatch,
                single_step.kind,
            );
        }
    }

    /// Fetch the reason the last debugger entry occurred.
    fn classify_stop(&mut self) {
        let pc = self.exception_ctx.program_counter;
        self.stop_reason = match self.hw_manager.last_break_reason() {
            // A hardware break while an async halt is armed is the sentinel mismatch firing on the
            // first user instruction, i.e. the response to a Ctrl-C. (The mismatch fires
            // immediately on resume, so when armed it's effectively always the cause.)
            Some(DebugEventReason::Breakpoint) if self.interrupt_pending => StopReason::Interrupt,
            Some(DebugEventReason::Breakpoint) => StopReason::HardwareBreak,
            Some(DebugEventReason::BkptInstr) => match self.query_sw_breakpoint(pc) {
                Some(id) => StopReason::TrackedSoftwareBreak { id },
                None => StopReason::UntrackedSoftwareBreak,
            },
            _ => StopReason::Other,
        }
    }

    /// Acknowledge a manual call to `bkpt` by proceeding to the next instruction.
    fn fixup_manual_bkpt(&mut self) {
        if self.stop_reason == StopReason::UntrackedSoftwareBreak {
            // Normally we try to avoid an infinite loop of breakpoints by replacing tracked
            // software breakpoints with their real instructions and re-running them. But if the
            // `bkpt` *is* the real instruction then we don't need to do the normal
            // replace-and-rerun thing. Instead, we just skip over it because its side-effect has
            // been completed.

            // SAFETY: Since the address was able to be properly fetched, it implies it is valid for
            // reads.
            let instr = unsafe { self.exception_ctx.read_instr() };
            self.exception_ctx.program_counter += instr.size() as u32;
        }
    }

    /// Returns the tracked software breakpoint occupying the given slot, if any.
    pub(crate) fn breakpoint(&self, id: usize) -> Option<SwBreakpoint> {
        self.breaks[id]
    }

    /// Returns whether breakpoints are currently prevented from triggering.
    pub(crate) fn breakpoints_ignored(&self) -> bool {
        self.breaks_paused
    }

    /// Get the working copy of the saved program context.
    pub(crate) fn saved_ctx(&self) -> &DebugEventContext {
        &self.exception_ctx
    }

    pub fn gdb_stop_reason(&self) -> MultiThreadStopReason<u32> {
        if self.monitor_status == MonitorStatus::Exiting {
            return MultiThreadStopReason::Exited(0);
        }

        match self.stop_reason {
            StopReason::HardwareBreak => {
                // We don't use MultiThreadStopReason::DoneStep because it doesn't send thread info
                // to GDB (DoneStep is just an alias for SIGTRAP without thread info). HwBreak is
                // essentially the same message but with thread info set.
                MultiThreadStopReason::HwBreak(System::current_thread())
            }
            // Sometimes GDB will try to skip hardcoded breakpoints it's already seen
            // recently, so we report those as traps instead.
            StopReason::TrackedSoftwareBreak { .. } => {
                MultiThreadStopReason::SwBreak(System::current_thread())
            }
            StopReason::UntrackedSoftwareBreak | StopReason::Other => {
                MultiThreadStopReason::SignalWithThread {
                    signal: Signal::SIGTRAP,
                    tid: System::current_thread(),
                }
            }
            // A host-requested async halt (GDB Ctrl-C) is reported as SIGINT, which is what GDB is
            // waiting for after sending the interrupt byte.
            StopReason::Interrupt => MultiThreadStopReason::SignalWithThread {
                signal: Signal::SIGINT,
                tid: System::current_thread(),
            },
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
    fn read_registers(&mut self, regs: &mut DebugEventContext) -> TargetResult<(), Self> {
        log::info!("Reading all registers");
        *regs = self.exception_ctx.clone();
        Ok(())
    }

    fn write_registers(&mut self, regs: &DebugEventContext) -> TargetResult<(), Self> {
        log::info!("Writing all registers");
        self.exception_ctx = regs.clone();
        Ok(())
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        log::info!("Read addr {start_addr} for {} bytes", data.len(),);

        let bytes_read = memory::read_memory(start_addr, data);
        if bytes_read == 0 {
            return Err(TargetError::Errno(HostIoErrno::EFAULT as u8));
        }

        Ok(bytes_read)
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        log::info!("Write addr {start_addr} for {} bytes", data.len(),);

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
