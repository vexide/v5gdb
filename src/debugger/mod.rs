//! Main debugger loop and event handling logic.

use core::{
    convert::Infallible,
    mem,
    sync::atomic::{AtomicU32, Ordering},
};

use derive_more::From;
use gdbstub::{
    conn::Connection,
    stub::{
        GdbStub, GdbStubBuilder, GdbStubError, MultiThreadStopReason, SingleThreadStopReason,
        state_machine::GdbStubStateMachine,
    },
};
use snafu::Snafu;
use spin::{Mutex, MutexGuard};
use static_cell::StaticCell;
use vex_sdk::vexSystemExitRequest;
use zynq7000::devcfg;

use crate::{
    Debugger,
    exceptions::DebugEventContext,
    gdb_target::{MonitorStatus, StopReason, V5Target},
    sys::{DebuggerSystem, System},
    transport::{Transport, TransportError},
};

#[derive(Debug, Snafu)]
pub enum DebuggerError {
    #[snafu(context(false))]
    Io { source: TransportError },
    #[snafu(context(false))]
    GdbStub {
        source: GdbStubError<Infallible, TransportError>,
    },
}

/// Debugger manager.
pub struct V5Debugger<S: Transport> {
    session: Mutex<DebugSession<'static, S>>,
}

impl<S: Transport> V5Debugger<S> {
    /// Creates a new debugger.
    ///
    /// This function can only be called once per program run because the debugger will attempt to
    /// claim a global packet buffer.
    #[must_use]
    pub fn new(stream: S) -> Self {
        const PACKET_BUFFER_SIZE: usize = 4096;
        // Stored as a global to help limit stack usage.
        static PACKET_BUFFER: StaticCell<[u8; PACKET_BUFFER_SIZE]> = StaticCell::new();

        let pkt_buffer = PACKET_BUFFER
            .try_init_with(|| [0; _])
            .expect("Tried to claim packet buffer twice");

        let target = V5Target::new(&mut unsafe { devcfg::Registers::new_mmio_fixed() });

        Self {
            session: Mutex::new(DebugSession {
                stage: SessionStage::Uninitialized(
                    GdbStubBuilder::new(stream)
                        .with_packet_buffer(pkt_buffer)
                        .build()
                        .unwrap(),
                ),
                target,
                internal_breaks: None,
            }),
        }
    }

    /// Returns the debugger's internal state.
    #[must_use]
    pub fn session<'a>(&'a self) -> MutexGuard<'a, DebugSession<'static, S>> {
        self.session.lock()
    }
}

unsafe impl<S: Transport + 'static> Debugger for V5Debugger<S> {
    fn initialize(&self) {
        let mut session = self.session();
        session.register_internal_breakpoints();
        System::initialize(&mut session.target);
        crate::sdk::competition::install_override();
        log::debug!("Debugger initialized");
    }

    unsafe fn handle_debug_event(&self, ctx: &mut DebugEventContext) -> bool {
        let mut session = self.session();

        let stop_reason = session.target.enter_breakpoint(ctx);

        let action = session.handle_stop(stop_reason);
        if action == StopAction::EnterMonitor {
            log::debug!("Starting debug console");
            session.run_debug_console();
            log::debug!("Debug console has exited");
        }

        session.target.leave_breakpoint(ctx)
    }

    fn poll(&self) {
        // This runs in an interrupt context so it must be non-blocking and must not write to
        // serial. Serial writes aren't IRQ-safe, and logger tend to write to stdout (serial ch1),
        // so also avoid logging here.

        // A failed try_lock means the monitor is active (or briefly busy). In that case the serial
        // stream carries real GDB protocol traffic so we shouldn't touch it.
        let Some(_session) = self.session.try_lock() else {
            return;
        };

        // While the program is running, GDB can send `0x03` (Interrupt) to request a pause.
        const CTRL_C: i32 = 0x03;
        if unsafe { vex_sdk::vexSerialPeekChar(1) } != CTRL_C {
            return;
        }

        // Consume the interrupt byte so it doesn't linger in the FIFO.
        unsafe {
            vex_sdk::vexSerialReadChar(1);
        }

        ASYNC_HALT_REQUESTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Number of Ctrl-C (`0x03`) async-halt requests observed by [`V5Debugger::poll`].
static ASYNC_HALT_REQUESTS: AtomicU32 = AtomicU32::new(0);

/// Returns how many async-halt (Ctrl-C) requests the debugger has received.
#[must_use]
pub fn async_halt_request_count() -> u32 {
    ASYNC_HALT_REQUESTS.load(Ordering::Relaxed)
}

/// What shall be done after a breakpoint has been acknowledged.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum StopAction {
    /// Return to the code that was previously executing.
    Resume,
    /// Pause the program and run the debug console.
    EnterMonitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalBreakpoint {
    SystemExitRequest,
}

/// Handles the GDB protocol lifecycle.
pub struct DebugSession<'a, S>
where
    S: Transport,
{
    pub target: V5Target,
    internal_breaks: Option<[(InternalBreakpoint, u32); 1]>,
    stage: SessionStage<'a, S>,
}

#[derive(From)]
enum SessionStage<'a, C: Connection> {
    /// Remote has not yet connected / been configured.
    Uninitialized(GdbStub<'a, V5Target, C>),
    /// Session is running.
    Active(GdbStubStateMachine<'a, V5Target, C>),
    /// Placeholder while transitioning between states.
    Transitioning,
}

impl<S> DebugSession<'_, S>
where
    S: Transport,
{
    fn has_client(&self) -> bool {
        match &self.stage {
            SessionStage::Active(GdbStubStateMachine::Disconnected(_)) => false,
            SessionStage::Active(_) => true,
            _ => false,
        }
    }

    /// Respond to entering a breakpoint and decide whether the user should be notified of it.
    fn handle_stop(&mut self, reason: StopReason) -> StopAction {
        // Tracked SW breakpoints are the only ones we ever allow to be "internal".
        let StopReason::TrackedSoftwareBreak { id } = reason else {
            return StopAction::EnterMonitor;
        };

        let breakpoint = self
            .target
            .breakpoint(id)
            .expect("bkpt deleted before it could be handled");

        // Internal breakpoint handlers might request to enter debug monitor.
        let internal_action = if breakpoint.reason.internal {
            self.handle_internal_breakpoint()
        } else {
            StopAction::Resume
        };

        // User-requested breakpoints always need to be shown because they were explicitly added.
        if breakpoint.reason.user {
            StopAction::EnterMonitor
        } else {
            internal_action
        }
    }

    /// Run the handler for the internal breakpoint at the current PC, if any.
    ///
    /// Returns whether the debugger should enter the monitor even when the user didn't ask to stop
    /// here.
    fn handle_internal_breakpoint(&mut self) -> StopAction {
        debug_assert!(self.target.breakpoints_ignored());

        let pc = self.target.saved_ctx().program_counter;

        let Some(&(id, addr)) = self
            .internal_breaks
            .iter()
            .flatten()
            .find(|&&(_id, addr)| addr == pc)
        else {
            return StopAction::Resume;
        };

        match id {
            // This handler allows us to disconnect the GDB client cleanly before actually exiting.
            InternalBreakpoint::SystemExitRequest => {
                self.target.remove_sw_breakpoint(addr, true);

                if !self.has_client() {
                    // If there's no client connected, exit as normal without trying to tell GDB.
                    return StopAction::Resume;
                }

                self.target.monitor_status = MonitorStatus::Exiting;

                // Continue to the debug monitor - once GDB realizes we are exiting, it will
                // disconnect and allow us to return back to calling vexSystemExitRequest.
                StopAction::EnterMonitor
            }
        }
    }

    fn register_internal_breakpoints(&mut self) {
        assert!(self.internal_breaks.is_none());

        let exit_func = vexSystemExitRequest as *const () as u32;
        let is_thumb = (exit_func & 1) != 0;
        log::debug!("Register pre-exit handler (thumb={is_thumb})");

        let internal_breaks = [(InternalBreakpoint::SystemExitRequest, exit_func & !1)];

        for (_id, addr) in internal_breaks {
            unsafe {
                self.target
                    .register_sw_breakpoint(addr, is_thumb, true)
                    .unwrap();
            }
        }

        self.internal_breaks = Some(internal_breaks);
    }

    /// Runs the debug console until the user indicates they want to continue program execution.
    fn run_debug_console(&mut self) {
        let stage = mem::replace(&mut self.stage, SessionStage::Transitioning);
        match stage {
            SessionStage::Uninitialized(gdb) => {
                self.stage = gdb.run_state_machine(&mut self.target).unwrap().into();
                self.run_debug_console();
            }
            SessionStage::Active(mut state) => {
                while self.target.monitor_status != MonitorStatus::ResumingProgram {
                    unsafe {
                        vex_sdk::vexTasksRun();
                    }

                    state = Self::tick_state_machine(state, &mut self.target)
                        .expect("debugger encountered an error");
                }

                self.stage = state.into();
            }
            SessionStage::Transitioning => panic!("Cannot resume from transitioning state"),
        }
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
                let reported_reason = target.gdb_stop_reason();
                log::info!("Debugger Stop reason: {reported_reason:?}");

                // Once we tell GDB we've exited we should exit the monitor because the session will
                // end.
                if matches!(reported_reason, MultiThreadStopReason::Exited(_)) {
                    target.monitor_status = MonitorStatus::ResumingProgram;
                }

                Ok(gdb.report_stop(target, reported_reason)?)
            }
            GdbStubStateMachine::CtrlCInterrupt(gdb) => {
                log::warn!("Got Ctrl+C");
                let stop_reason: Option<SingleThreadStopReason<_>> = None;
                Ok(gdb.interrupt_handled(target, stop_reason)?)
            }
            GdbStubStateMachine::Disconnected(gdb) => {
                target.monitor_status = MonitorStatus::ResumingProgram;
                Ok(gdb.return_to_idle())
            }
        }
    }
}
