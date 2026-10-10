#![allow(missing_docs)]
#![no_std]
#![cfg_attr(not(target_arch = "arm"), allow(unused))]

#[cfg(feature = "alloc")]
extern crate alloc;

use core::any::Any;

use spin::Once;

use crate::exceptions::DebugEventContext;

pub mod cpu;
pub mod exceptions;
pub mod logging;
mod sys;
pub mod transport;

cfg_select! {
    target_arch = "arm" => {
        pub mod gdb_target;
        mod sdk;
        pub mod debugger;
    }
    _ => {
        pub use debugger_stub as debugger;
    }
}

#[allow(dead_code, reason = "only used on non-armv7a")]
mod debugger_stub {
    use crate::{Debugger, transport::Transport};

    pub struct V5Debugger<S: Transport> {
        _stream: spin::Mutex<S>,
        config: DebuggerConfig,
    }

    /// Init configuration for the debugger.
    ///
    /// These values control the debugger's default behaviour and are applied once during
    /// [`Debugger::initialize`]. Settings can be overridden at runtime through GDB monitor
    /// commands after the debugger starts
    #[derive(Debug, Default, Clone)]
    pub struct DebuggerConfig {
        /// Whether motors should be stopped by default when a breakpoint fires.
        ///
        /// When `true`, [`sdk::stop_all_motors`] is called immediately on every breakpoint before
        /// the GDB console loop begins. This can be overridden at runtime with
        /// `monitor autostop true` / `monitor autostop false`.
        ///
        /// Defaults to `false`.
        pub stop_motors_on_break: bool,
    }

    impl<S: Transport> V5Debugger<S> {
        /// Creates a new debugger with default configuration.
        #[must_use]
        pub fn new(stream: S) -> Self {
            Self {
                _stream: spin::Mutex::new(stream),
                config: DebuggerConfig::default(),
            }
        }

        /// Sets whether all motors should be automatically stopped whenever a breakpoint fires.
        ///
        /// This controls the *default* value of `V5Target::stop_motors_on_break`. It is applied
        /// once at initialisation and has no effect if changed after [`install`](crate::install)
        /// is called. Use `monitor autostop true` / `monitor autostop false` from GDB to toggle
        /// the setting at runtime.
        ///
        /// Defaults to `false`.
        #[must_use]
        pub fn with_motor_stop(mut self, enabled: bool) -> Self {
            self.config.stop_motors_on_break = enabled;
            self
        }
    }

    unsafe impl<S: Transport + Send + 'static> Debugger for V5Debugger<S> {
        fn initialize(&self) {}

        unsafe fn handle_debug_event(
            &self,
            _ctx: &mut crate::exceptions::DebugEventContext,
        ) -> bool {
            unimplemented!()
        }
    }
}

pub static DEBUGGER: Once<&dyn Debugger> = Once::new();

/// Debugger implementation.
///
/// # Safety
///
/// The debugger must not corrupt the CPU state when handling debug events.
pub unsafe trait Debugger: Send + Sync + Any {
    /// Initializes the debugger.
    fn initialize(&self);

    /// A callback function which is run whenever a breakpoint is triggered.
    ///
    /// The function is given access to the pre-breakpoint CPU state and can view/modify it as
    /// needed.
    ///
    /// Returns whether the system scheduler should be unpaused when returning to user code.
    ///
    /// # Safety
    ///
    /// The given fault must represent valid, saved CPU state.
    unsafe fn handle_debug_event(&self, ctx: &mut DebugEventContext) -> bool;

    /// An asynchronous callback which is run from an IRQ handler every few milliseconds.
    ///
    /// This is used to maintain the debugger and works even if user code becomes stuck or
    /// misbehaves. Since this is called from an interrupt context, it must be non-blocking and
    /// avoid operations that aren't thread-safe (such as writing to serial).
    fn poll(&self) {}
}

/// Set the current debugger.
///
/// This will move the given debugger onto the heap, so it's more expensive than [`install_by_ref`].
#[cfg(feature = "alloc")]
pub fn install(debugger: impl Debugger + 'static) {
    use alloc::boxed::Box;
    install_by_ref(Box::leak(Box::new(debugger)));
}

/// Set the current debugger, by reference.
pub fn install_by_ref(debugger: &'static dyn Debugger) {
    logging::debug!("Installing debugger");
    assert!(!DEBUGGER.is_completed(), "A debugger is already installed.");
    DEBUGGER.call_once(|| debugger);

    #[cfg(target_arch = "arm")]
    exceptions::install_vectors();

    DEBUGGER.get().unwrap().initialize();
}

/// Manually trigger a breakpoint.
///
/// This should only be run if a debugger is installed. If no debugger is installed, this will
/// crash your program instead of pausing it.
#[macro_export]
macro_rules! breakpoint {
    () => {
        #[cfg(target_arch = "arm")]
        unsafe {
            ::core::arch::asm!("bkpt", options(nostack, nomem, preserves_flags));
        }
    };
}
