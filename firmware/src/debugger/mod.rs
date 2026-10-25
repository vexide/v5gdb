//! Main debugger loop and event handling logic.

use core::{mem, ptr::NonNull};

use gdbstub::{
    conn::{Connection, ConnectionExt},
    stub::{
        GdbStub, GdbStubBuilder, GdbStubError, MultiThreadStopReason, SingleThreadStopReason,
        state_machine::GdbStubStateMachine,
    },
};
use spin::{Mutex, MutexGuard, Once};
use static_cell::ConstStaticCell;
use thiserror::Error;
use vex_sdk::vexSystemExitRequest;
use zynq7000::devcfg;

use crate::{
    Debugger, SchedulerAction,
    exceptions::DebugEventContext,
    gdb_target::{MonitorStatus, StopReason, V5Target, error::FatalError},
    logging::{self, LevelFilter},
    sdk::stop_all_motors,
    sys::{DebuggerSystem, System},
    transport::{
        StdioTransport, Transport, TransportError,
        link::{Link, LinkHandle},
        mux,
    },
};

const PACKET_BUFFER_SIZE: usize = 4096;
const TX_BUFFER_SIZE: usize = 2 * PACKET_BUFFER_SIZE;

// These are stored as globals (in .bss) to help limit stack usage.

/// Used by gdbstub to build responses and decode incoming data.
static PACKET_BUFFER: ConstStaticCell<[u8; PACKET_BUFFER_SIZE]> =
    ConstStaticCell::new([0; PACKET_BUFFER_SIZE]);
/// Holds incoming packets before they are passed along to gdbstub.
static RX_BUFFER: ConstStaticCell<[u8; PACKET_BUFFER_SIZE]> =
    ConstStaticCell::new([0; PACKET_BUFFER_SIZE]);
/// Holds the last outgoing packet so it can be sent again if the client NACKs.
static TX_BUFFER: ConstStaticCell<[u8; TX_BUFFER_SIZE]> = ConstStaticCell::new([0; TX_BUFFER_SIZE]);

#[derive(Debug, Error)]
pub enum DebuggerError {
    #[error("i/o error")]
    Io(#[from] TransportError),
    #[error(transparent)]
    GdbStub(#[from] GdbStubError<FatalError, TransportError>),
}

/// Initial configuration for [`V5Debugger`].
///
/// These values are applied once during [`Debugger::initialize`]. Most settings can also be changed
/// at runtime from GDB using monitor commands.
///
/// ```ignore
/// let config = DebuggerConfig::new()
///     .stop_motors_on_break(true)
///     .log_level(LevelFilter::Info);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DebuggerConfig {
    /// Whether all motors should be stopped immediately when a breakpoint fires.
    ///
    /// This prevents the robot from driving away or actuating mechanisms while execution is
    /// paused. Can be toggled at runtime with `monitor autostop true` / `monitor autostop false`.
    ///
    /// Defaults to `false`.
    pub stop_motors_on_break: bool,
    /// The most verbose level of debugger log messages that will be printed.
    ///
    /// If `None`, the level set by [`logging::set_max_level`] is left as is. Can be changed at
    /// runtime with `monitor log <level>`.
    ///
    /// Defaults to `None`.
    pub log_level: Option<LevelFilter>,
}

impl DebuggerConfig {
    /// Creates a config with default settings.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stop_motors_on_break: false,
            log_level: None,
        }
    }

    /// Sets [`stop_motors_on_break`](Self::stop_motors_on_break).
    #[must_use]
    pub const fn stop_motors_on_break(mut self, enabled: bool) -> Self {
        self.stop_motors_on_break = enabled;
        self
    }

    /// Sets [`log_level`](Self::log_level).
    #[must_use]
    pub const fn log_level(mut self, level: LevelFilter) -> Self {
        self.log_level = Some(level);
        self
    }
}

impl Default for DebuggerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Debugger manager.
///
/// Create one with [`V5Debugger::new`] and pass it to [`install`](crate::install).
pub struct V5Debugger<S: Transport> {
    /// The transport passed to [`V5Debugger::new`].
    ///
    /// It's moved into [`Self::link`] when the debugger is initialized.
    transport: Mutex<Option<S>>,
    /// The connection to GDB.
    ///
    /// gdbstub accesses this through a [`LinkHandle`] so the connection isn't lost if the stub
    /// gets in a bad state (i.e. has a fatal error). This is set by [`Debugger::initialize`].
    link: Once<Mutex<Link<S>>>,
    /// The debugger's internal state. This is set by [`Debugger::initialize`].
    session: Once<Mutex<DebugSession<'static, S>>>,
    /// Initial settings applied to the debugger on [`Debugger::initialize`].
    ///
    /// After initialization, the live values in [`V5Target`] are the source of truth and this
    /// field is no longer read.
    config: DebuggerConfig,
}

impl V5Debugger<StdioTransport> {
    /// Creates a new debugger that communicates with GDB over the V5's USB serial port.
    #[must_use]
    pub const fn stdio() -> Self {
        Self::new(StdioTransport::new())
    }
}

impl<S: Transport> V5Debugger<S> {
    /// Creates a new debugger with default config.
    ///
    /// The debugger doesn't touch any hardware or claim any global resources until it's installed.
    #[must_use]
    pub const fn new(transport: S) -> Self {
        Self {
            transport: Mutex::new(Some(transport)),
            link: Once::new(),
            session: Once::new(),
            config: DebuggerConfig::new(),
        }
    }

    /// Applies a [`DebuggerConfig`] to this debugger, replacing any previous configuration.
    #[must_use]
    pub fn with_config(mut self, config: DebuggerConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets whether all motors should be stopped immediately when a breakpoint fires.
    ///
    /// See [`DebuggerConfig::stop_motors_on_break`].
    #[must_use]
    pub const fn stop_motors_on_break(mut self, enabled: bool) -> Self {
        self.config.stop_motors_on_break = enabled;
        self
    }

    /// Sets the most verbose level of debugger log messages that will be printed.
    ///
    /// See [`DebuggerConfig::log_level`].
    #[must_use]
    pub const fn log_level(mut self, level: LevelFilter) -> Self {
        self.config.log_level = Some(level);
        self
    }

    /// Returns the debugger's internal state.
    fn session(&self) -> MutexGuard<'_, DebugSession<'static, S>> {
        self.session
            .get()
            .expect("debugger should be initialized")
            .try_lock()
            .expect("should not recursively enter debug monitor")
    }
}

unsafe impl<S: Transport> Debugger for V5Debugger<S> {
    fn initialize(&'static self) {
        if let Some(level) = self.config.log_level {
            logging::set_max_level(level);
        }

        let transport = self
            .transport
            .lock()
            .take()
            .expect("debugger should only be initialized once");

        // Now that we have a 'static reference to the debugger, we can save a reference to the
        // link which we can reuse in the future if the gdbstub instance gets in a bad state and
        // loses the one we give it initially.
        let link = self
            .link
            .call_once(|| Mutex::new(Link::new(transport, RX_BUFFER.take(), TX_BUFFER.take())));
        let mut stub_config = StubConfig::new(PACKET_BUFFER.take());
        stub_config.link = Some(link);

        let target = V5Target::new(&mut unsafe { devcfg::Registers::new_mmio_fixed() });
        self.session.call_once(|| {
            Mutex::new(DebugSession {
                stage: SessionStage::NotStarted,
                stub_config,
                target,
                internal_breaks: None,
            })
        });

        let mut session = self.session();
        session.register_internal_breakpoints();
        System::initialize(&mut session.target);
        crate::sdk::competition::install_override();

        // Apply the initial config into the live target state. from this point on, the values in
        // V5Target are the source of truth so `self.config` is never read again.
        session.target.stop_motors_on_break = self.config.stop_motors_on_break;

        // Start the transport session & GDB state machine now so we can start receiving Ctrl-C
        // packets, even if we haven't hit a breakpoint yet.
        session.restart();

        logging::debug!("Debugger initialized (config={:?})", self.config);
    }

    unsafe fn handle_debug_event(&self, ctx: &mut DebugEventContext) -> SchedulerAction {
        let mut session = self.session();

        let stop_reason = session.target.enter_breakpoint(ctx);

        mux::warn_if_user_read();

        if session.target.stop_motors_on_break {
            logging::debug!("Auto motor-stop triggered by breakpoint");
            stop_all_motors();
        }

        let action = session.handle_stop(stop_reason);
        if action == StopAction::EnterMonitor {
            logging::debug!("Starting debug console");
            session.run_debug_console();
            logging::debug!("Debug console has exited");
        }

        session.target.leave_breakpoint(ctx)
    }

    fn poll(&self) {
        // This runs in an interrupt context so it must be non-blocking and must not write to
        // serial. Serial writes aren't IRQ-safe, and loggers tend to write to stdout (serial ch1),
        // so also avoid logging here.

        // The session doesn't exist until the debugger has been initialized. A failed try_lock
        // means the monitor is active. In that case the serial stream carries real GDB protocol
        // traffic so we shouldn't touch it.
        let Some(mut session) = self.session.get().and_then(Mutex::try_lock) else {
            return;
        };

        // If an interrupt is already pending then there's no need to scan transport data in the
        // background, since we're about to handle it anyways.
        if session.target.interrupt_pending() {
            return;
        }

        let SessionStage::Active(gdb_state) = &mut session.stage else {
            return;
        };

        let conn = match gdb_state {
            GdbStubStateMachine::Running(gdb) => gdb.borrow_conn(),
            // "Idle" while the debug monitor is inactive means no client has connected yet.
            GdbStubStateMachine::Idle(gdb) => gdb.borrow_conn(),
            _ => return,
        };

        const CTRL_C: u8 = 0x03;

        // While the program is running, GDB can send `0x03` (Interrupt) to request a pause.
        while let Ok(Some(byte)) = conn.try_read() {
            if byte == CTRL_C {
                session.target.request_interrupt();
                break;
            }
        }
    }
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
enum InternalBreakpoint {
    SystemExitRequest,
}

/// The resources that a gdbstub instance needs.
///
/// This is kept around even after the session is initialized so a new instance can be created if
/// the current one gets in a bad state (e.g. due to a malformed packet).
struct StubConfig<S: 'static> {
    /// The debugger's link.
    ///
    /// This is `None` until the debugger is initialized, since a `'static` reference to the
    /// debugger (and, by extension, the Link) isn't available until it's been installed.
    link: Option<&'static Mutex<Link<S>>>,
    /// gdbstub's packet buffer, for building responses and decoding incoming data.
    ///
    /// This is a pointer because gdbstub needs an *actual* long-lived `&mut` reference to it,
    /// which is then lost whenever a fatal error destroys the gdbstub instance - and thus we can't
    /// use a RefCell or similar to avoid unsafe.
    packet_buffer: NonNull<[u8]>,
}

impl<S: 'static + Transport> StubConfig<S> {
    pub fn new(packet_buf: &'static mut [u8]) -> Self {
        Self {
            link: None,
            packet_buffer: NonNull::from(packet_buf),
        }
    }

    /// Creates a new [`GdbStub`] instance using this config.
    ///
    /// Calling this multiple times allows re-using the same packet buffer and link even if the
    /// original references to them are lost by gdbstub.
    ///
    /// # Safety
    ///
    /// Any previous GdbStub built using this config must have been dropped.
    pub unsafe fn build(&mut self) -> GdbStub<'static, V5Target, LinkHandle<S>> {
        let link = LinkHandle::new(
            self.link
                .expect("debugger should be initialized before starting a session"),
        );

        // SAFETY: The packet buffer is a static and it's not being used by any other gdbstub
        // instance.
        let packet_buffer = unsafe { self.packet_buffer.as_mut() };

        GdbStubBuilder::new(link)
            .with_packet_buffer(packet_buffer)
            .build()
            .unwrap()
    }
}

/// Handles the GDB protocol lifecycle.
struct DebugSession<'a, S>
where
    S: Transport,
{
    target: V5Target,
    internal_breaks: Option<[(InternalBreakpoint, u32); 1]>,
    stage: SessionStage<'a, S>,
    stub_config: StubConfig<S>,
}

#[derive(Default)]
enum SessionStage<'a, S: Transport> {
    /// There's no gdbstub instance, either because the debugger hasn't been initialized yet or
    /// because starting a session failed.
    NotStarted,
    /// Session is running.
    Active(GdbStubStateMachine<'a, V5Target, LinkHandle<S>>),
    /// Placeholder while transitioning between states.
    #[default]
    Transitioning,
}

// SAFETY: `packet_buffer` points to a static that is only accessed by the session's gdbstub
// instance.
unsafe impl<S: Send> Send for StubConfig<S> {}

impl<'a, S> DebugSession<'a, S>
where
    S: Transport,
{
    /// Starts a new GDB session, discarding the current one (if any).
    ///
    /// gdbstub's protocol state (negotiated features, selected threads) is lost, so a GDB client
    /// that was connected to the old session needs to reconnect.
    fn restart(&mut self) {
        // Drop the current gdbstub instance before creating a new one, since they'd share the link
        // and the packet buffer.
        self.stage = SessionStage::Transitioning;

        // SAFETY: We've dropped the old gdbstub instance, so we can re-use these resources.
        let gdb = unsafe { self.stub_config.build() };
        self.stage = match gdb.run_state_machine(&mut self.target) {
            Ok(state) => SessionStage::Active(state),
            Err(err) => {
                logging::error!("Failed to start a GDB session: {err}");
                SessionStage::NotStarted
            }
        };
    }

    /// Returns whether a GDB client is connected.
    ///
    /// Only valid outside the debug monitor. The session is in `Running` while a client has
    /// resumed the program and `Idle` before any client has connected. Disconnects only happen
    /// inside the debug monitor, which stays active until a new client resumes the program.
    fn has_client(&self) -> bool {
        matches!(
            self.stage,
            SessionStage::Active(GdbStubStateMachine::Running(_))
        )
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
        logging::debug!("Registering pre-exit handler at {exit_func:#010x} (thumb={is_thumb})");

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
        while self.target.monitor_status != MonitorStatus::ResumingProgram {
            unsafe {
                vex_sdk::vexTasksRun();
            }

            match mem::take(&mut self.stage) {
                SessionStage::Active(state) => {
                    match Self::tick_state_machine(state, &mut self.target) {
                        Ok(state) => self.stage = SessionStage::Active(state),
                        Err(err) => {
                            // gdbstub can't continue after a fatal error, so start over instead of
                            // taking down the user's program. The program stays paused until a
                            // GDB client reconnects.
                            logging::error!(
                                "Debug session ended due to an error, please reconnect: {err}"
                            );
                            self.restart();
                        }
                    }
                }
                SessionStage::NotStarted => self.restart(),
                SessionStage::Transitioning => panic!("Cannot resume from transitioning state"),
            }
        }

        // gdbstub doesn't flush after acknowledging a resume packet, so send the ack before the
        // program starts running.
        if let SessionStage::Active(GdbStubStateMachine::Running(gdb)) = &mut self.stage {
            let _ = gdb.borrow_conn().flush();
        }
    }

    fn tick_state_machine(
        gdb: GdbStubStateMachine<'a, V5Target, LinkHandle<S>>,
        target: &mut V5Target,
    ) -> Result<GdbStubStateMachine<'a, V5Target, LinkHandle<S>>, DebuggerError> {
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
                logging::info!("Reporting stop to client: {reported_reason:x?}");

                // Once we tell GDB we've exited we should exit the monitor because the session will
                // end.
                if matches!(reported_reason, MultiThreadStopReason::Exited(_)) {
                    target.monitor_status = MonitorStatus::ResumingProgram;
                }

                Ok(gdb.report_stop(target, reported_reason)?)
            }
            GdbStubStateMachine::CtrlCInterrupt(gdb) => {
                logging::warn!(
                    "Received a request to pause the program while it was already paused."
                );
                let stop_reason: Option<SingleThreadStopReason<_>> = None;
                Ok(gdb.interrupt_handled(target, stop_reason)?)
            }
            GdbStubStateMachine::Disconnected(gdb) => {
                logging::info!("GDB disconnected: {:?}", gdb.get_reason());
                // If we're about to exit the program, then GDB is probably disconnecting to let us
                // do that. In that case, unpause and actually exit. Otherwise, stay in the debug
                // monitor so a new GDB instance can pick up where we left off.
                if target.monitor_status == MonitorStatus::Exiting {
                    target.monitor_status = MonitorStatus::ResumingProgram;
                }
                Ok(gdb.return_to_idle())
            }
        }
    }
}
