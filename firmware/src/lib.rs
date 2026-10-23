//! GDB-compatible debugger backend for the VEX V5.
//!
//! Install the debugger at the start of your program:
//!
//! ```ignore
//! v5gdb::install(V5Debugger::stdio());
//! ```
//!
//! Then connect to the V5 Brain with the `v5gdb` CLI.

#![no_std]

#[cfg(not(target_arch = "arm"))]
compile_error!("v5gdb only supports the `armv7a-vex-v5` and `armv7a-none-eabi` targets.");

#[cfg(feature = "alloc")]
extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Once;

use crate::exceptions::DebugEventContext;

#[doc(hidden)]
pub mod cpu;
pub mod debugger;
#[doc(hidden)]
pub mod exceptions;
#[doc(hidden)]
pub mod gdb_target;
pub mod logging;
mod sdk;
mod sys;
pub mod transport;

pub use debugger::{DebuggerConfig, V5Debugger};

/// Common imports for using the debugger.
pub mod prelude {
    pub use crate::{
        DebuggerConfig, V5Debugger, breakpoint, logging::LevelFilter, transport::StdioTransport,
    };
}

/// The installed debugger.
pub(crate) static DEBUGGER: Once<&dyn Debugger> = Once::new();

/// Set once the installed debugger has finished initializing.
static READY: AtomicBool = AtomicBool::new(false);

/// What the system scheduler should do when the program resumes after a debug event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerAction {
    /// Unpause the scheduler.
    Resume,
    /// Keep the scheduler paused, e.g. while single-stepping a thread.
    StayPaused,
}

/// Debugger implementation.
///
/// # Safety
///
/// The debugger must not corrupt the CPU state when handling debug events.
pub unsafe trait Debugger: Send + Sync {
    /// Initializes the debugger.
    ///
    /// This is called once the debugger has been installed, so it can rely on `self` never moving.
    fn initialize(&'static self);

    /// A callback function which is run whenever a breakpoint is triggered.
    ///
    /// The function is given access to the pre-breakpoint CPU state and can view/modify it as
    /// needed.
    ///
    /// Returns what the system scheduler should do when returning to user code.
    ///
    /// # Safety
    ///
    /// The given fault must represent valid, saved CPU state.
    ///
    /// This function must be called with interrupts disabled. The implementation may re-enable them
    /// during the function call, but they will be disabled again before returning.
    unsafe fn handle_debug_event(&self, ctx: &mut DebugEventContext) -> SchedulerAction;

    /// An asynchronous callback which is run from an IRQ handler every few milliseconds.
    ///
    /// This is used to maintain the debugger and works even if user code becomes stuck or
    /// misbehaves. Since this is called from an interrupt context, it must be non-blocking and
    /// avoid operations that aren't thread-safe (such as writing to serial).
    fn poll(&self) {}
}

/// Installs the debugger.
///
/// This moves the given debugger onto the heap. Without the `alloc` feature, use
/// [`install_by_ref`] instead.
///
/// Only the first call to an `install` function has an effect. Later calls log a warning and
/// otherwise do nothing.
#[cfg(feature = "alloc")]
pub fn install(debugger: impl Debugger + 'static) {
    use alloc::boxed::Box;

    if DEBUGGER.is_completed() {
        logging::warn!("A debugger is already installed.");
        return;
    }
    install_by_ref(Box::leak(Box::new(debugger)));
}

/// Installs the debugger, by reference.
///
/// Only the first call to an `install` function has an effect. Later calls log a warning and
/// otherwise do nothing.
pub fn install_by_ref(debugger: &'static dyn Debugger) {
    let mut first = false;
    DEBUGGER.call_once(|| {
        first = true;
        debugger
    });
    if !first {
        logging::warn!("Installing multiple debuggers is not supported.");
        return;
    }

    logging::debug!("Installing debugger");
    exceptions::install_vectors();
    debugger.initialize();
    READY.store(true, Ordering::Release);
}

/// Installs a debugger that communicates with GDB over the V5's USB serial port.
///
/// This is the same as `install(V5Debugger::stdio())`, but it doesn't require the `alloc`
/// feature.
pub fn install_stdio() {
    static STDIO_DEBUGGER: V5Debugger<transport::StdioTransport> = V5Debugger::stdio();
    install_by_ref(&STDIO_DEBUGGER);
}

/// Returns whether a debugger has been installed and initialized.
#[must_use]
pub fn is_installed() -> bool {
    READY.load(Ordering::Acquire)
}

/// Pauses the program and enters the debugger, if one is installed.
///
/// If no debugger is installed, this does nothing. See [`breakpoint_unchecked!`] for a version that
/// skips the check.
#[macro_export]
macro_rules! breakpoint {
    () => {
        if $crate::is_installed() {
            $crate::breakpoint_unchecked!();
        }
    };
}

/// Pauses the program and enters the debugger.
///
/// This doesn't check whether a debugger is installed. If none is, this will crash your program
/// instead of pausing it.
#[macro_export]
macro_rules! breakpoint_unchecked {
    () => {
        unsafe {
            ::core::arch::asm!("bkpt", options(nostack, nomem, preserves_flags));
        }
    };
}
