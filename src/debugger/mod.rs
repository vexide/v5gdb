//! Main debugger loop and event handling logic.

use core::{
    convert::Infallible,
    sync::atomic::{AtomicBool, Ordering},
};

use gdbstub::{
    conn::{Connection, ConnectionExt},
    stub::{
        GdbStub, GdbStubBuilder, GdbStubError, MultiThreadStopReason, SingleThreadStopReason,
        state_machine::GdbStubStateMachine,
    },
};
use snafu::Snafu;
use spin::{Mutex, MutexGuard, Once};
use zynq7000::devcfg::DevCfg;

use crate::{
    Debugger,
    cpu::debug::DebugEventReason,
    debugger::sdk::InternalBreakpoint,
    exceptions::DebugEventContext,
    gdb_target::{V5Target, breakpoint::hardware::Specificity},
    sys::{DebuggerSystem, System},
    transport::TransportError,
};

pub mod sdk;

#[derive(Debug, Snafu)]
pub enum DebuggerError {
    #[snafu(context(false))]
    Io { source: TransportError },
    GdbStub {
        inner: GdbStubError<Infallible, TransportError>,
    },
}

impl From<GdbStubError<Infallible, TransportError>> for DebuggerError {
    fn from(value: GdbStubError<Infallible, TransportError>) -> Self {
        Self::GdbStub { inner: value }
    }
}

/// Debugger manager.
pub struct V5Debugger<S>
where
    S: Connection<Error = TransportError> + ConnectionExt,
{
    state: Mutex<DebuggerState<'static, S>>,
}

impl<S> V5Debugger<S>
where
    S: Connection<Error = TransportError> + ConnectionExt,
{
    /// Creates a new debugger.
    #[must_use]
    pub fn new(stream: S) -> Self {
        const GDB_PACKET_BUFFER_SIZE: usize = 4096;
        static mut GDB_PACKET_BUFFER: [u8; GDB_PACKET_BUFFER_SIZE] = [0; _];
        static GDB_PACKET_BUFFER_CLAIMED: AtomicBool = AtomicBool::new(false);

        if GDB_PACKET_BUFFER_CLAIMED.swap(true, Ordering::Acquire) {
            panic!("Cannot create multiple debuggers");
        }

        // SAFETY: The mutable ownership over the buffer can only be taken once.
        let gdb_buffer = unsafe {
            core::slice::from_raw_parts_mut(&raw mut GDB_PACKET_BUFFER[0], GDB_PACKET_BUFFER_SIZE)
        };

        let target = V5Target::new(&mut unsafe { DevCfg::new_mmio_fixed() });

        Self {
            state: Mutex::new(DebuggerState {
                gdb: {
                    Some(
                        GdbStubBuilder::new(stream)
                            .with_packet_buffer(gdb_buffer)
                            .build()
                            .unwrap(),
                    )
                },
                stub: None,
                target,
                internal_breaks: None,
            }),
        }
    }

    /// Returns the debugger's internal state.
    #[must_use]
    pub fn state<'a>(&'a self) -> MutexGuard<'a, DebuggerState<'static, S>> {
        self.state.lock()
    }
}

unsafe impl<S> Debugger for V5Debugger<S>
where
    S: Connection<Error = TransportError> + ConnectionExt + Send + 'static,
{
    fn initialize(&self) {
        let mut state = self.state();
        state.register_internal_breakpoints();
        System::initialize(&mut state.target);
        crate::sdk::competition::install_override();
        log::debug!("Debugger initialized");
    }

    unsafe fn handle_debug_event(&self, ctx: &mut DebugEventContext) {
        let mut state = self.state();
        // Pause software breakpoints before allowing unpredictable control flow (by interrupts).
        state.target.set_breakpoints_ignored(true);

        // We re-enable interrupts after the abort (so that UART works) but prevent the RTOS from
        // preempting us. When the debugger is active, the system should appear paused.
        System::suspend_preemption();
        unsafe {
            aarch32_cpu::interrupt::enable();
        }

        log::debug!("Entered debug event handler");
        static BKPT_LOG: Once = Once::new();
        BKPT_LOG.call_once(|| {
            log::error!("**** v5gdb: BREAKPOINT TRIGGERED ****");
            log::error!("Your program has been paused. Please connect a debugger.")
        });

        let was_locked = state.target.hw_manager.locked();
        state.target.hw_manager.set_locked(false);
        state.target.exception_ctx = ctx.clone();

        let reason = state.target.hw_manager.last_break_reason();

        let bkpt_address = state.target.exception_ctx.program_counter;
        let tracked_bkpt_id = state.target.query_sw_breakpoint(bkpt_address);

        let is_manual_bkpt =
            tracked_bkpt_id.is_none() && reason == Some(DebugEventReason::BkptInstr);

        // If we previously wanted to single step, we can permanently remove the breakpoint that
        // supported that now. The saved single step request isn't removed yet so that the stop
        // reason we report to GDB is correct.
        if let Some(single_step) = state.target.single_step_request {
            state.target.hw_manager.remove_breakpoint_at(
                single_step.target_addr,
                Specificity::Mismatch,
                single_step.kind,
            );
        }

        if is_manual_bkpt {
            // Normally we try to avoid an infinite loop of breakpoints by replacing tracked
            // software breakpoints with their real instructions and re-running them. But if the
            // `bkpt` *is* the real instruction then we don't need to do the normal
            // replace-and-rerun thing. Instead, we just skip over it because its side-effect has
            // been completed.

            // SAFETY: Since the address was able to be properly fetched, it implies it is valid for
            // reads.
            let instr = unsafe { state.target.exception_ctx.read_instr() };
            state.target.exception_ctx.program_counter += instr.size() as u32;
        }

        let mut show_debug_console = true;

        if let Some(id) = tracked_bkpt_id
            && let Some(bkpt) = state.target.breaks[id]
        {
            // Some tracked breakpoints weren't requested by the user and are just used internally.
            // These should be transparent to the user by default. Note: It's possible
            // for a breakpoint to be both requested by the user and used internally.
            show_debug_console = bkpt.reason.user;

            // If this breakpoint is used internally, run any necessary callbacks.
            if bkpt.reason.internal {
                show_debug_console |= state.handle_internal_breakpoint();
            }
        }

        if show_debug_console {
            log::debug!("Starting debug console");
            state.run_debug_console();
            log::debug!("Debug console has exited");
        }

        // Write any modifications back to the stack so the assembly code restores the updated state
        *ctx = state.target.exception_ctx.clone();

        log::debug!("Exiting debug event handler");

        state.target.hw_manager.set_locked(was_locked);
        state.target.set_breakpoints_ignored(false);

        // Disable interrupts again so that the restore code doesn't get accidentally preempted.
        // Resuming the system scheduler is handled by the callee, so we can skip doing that.
        aarch32_cpu::interrupt::disable();
    }
}

/// Internal mutable state of debugger.
pub struct DebuggerState<'a, S>
where
    S: Connection<Error = TransportError> + ConnectionExt,
{
    pub target: V5Target,
    internal_breaks: Option<[(InternalBreakpoint, u32); 1]>,
    gdb: Option<GdbStub<'a, V5Target, S>>,
    stub: Option<GdbStubStateMachine<'a, V5Target, S>>,
}

impl<S> DebuggerState<'_, S>
where
    S: Connection<Error = TransportError> + ConnectionExt,
{
    fn has_client(&self) -> bool {
        let disconnected = matches!(
            &self.stub,
            None | Some(GdbStubStateMachine::Disconnected(_))
        );
        !disconnected
    }

    /// Runs the debug console until the user indicates they want to continue program execution.
    fn run_debug_console(&mut self) {
        if let Some(gdb) = self.gdb.take() {
            // Initial GDB setup - calls connection setup callback.
            self.stub = Some(gdb.run_state_machine(&mut self.target).unwrap());
        }
        let mut gdb = self.stub.take().unwrap();

        // Enter debugging loop until it's time to resume.

        self.target.reset_resume();
        while !self.target.resume {
            unsafe {
                vex_sdk::vexTasksRun();
            }

            gdb = Self::tick_state_machine(gdb, &mut self.target)
                .expect("debugger encountered an error");
        }

        self.target.resume = false;
        self.stub = Some(gdb);
    }

    fn tick_state_machine<'a>(
        gdb: GdbStubStateMachine<'a, V5Target, S>,
        target: &mut V5Target,
    ) -> Result<GdbStubStateMachine<'a, V5Target, S>, DebuggerError> {
        match gdb {
            GdbStubStateMachine::Idle(mut gdb) => {
                if let Ok(byte) = gdb.borrow_conn().read() {
                    Ok(gdb.incoming_data(target, byte)?)
                } else {
                    Ok(gdb.into())
                }
            }
            GdbStubStateMachine::Running(gdb) => {
                let reported_reason = target.get_stop_reason();
                target.single_step_request = None;

                log::warn!("Debugger Stop reason: {reported_reason:?}");

                // Once we tell GDB we've exited we should exit the monitor because the session will
                // end.
                if matches!(reported_reason, MultiThreadStopReason::Exited(_)) {
                    target.resume = true;
                }

                Ok(gdb.report_stop(target, reported_reason)?)
            }
            GdbStubStateMachine::CtrlCInterrupt(gdb) => {
                log::warn!("Got Ctrl+C");
                let stop_reason: Option<SingleThreadStopReason<_>> = None;
                Ok(gdb.interrupt_handled(target, stop_reason)?)
            }
            GdbStubStateMachine::Disconnected(gdb) => Ok(gdb.return_to_idle()),
        }
    }
}
