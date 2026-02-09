use cfg_if::cfg_if;
use gdbstub::{
    common::Tid,
    target::{
        TargetError,
        ext::{host_io::HostIoErrno, monitor_cmd::ConsoleOutput},
    },
};
use snafu::Snafu;

use crate::gdb_target::{
    V5Target,
    arch::{ArmRegisterID, ArmRegisters},
    single_register_access::SavedRegister,
};

cfg_if! {
    if #[cfg(feature = "freertos")] {
        pub mod freertos;
        pub type System = freertos::FreeRtosSystem;
    } else {
        pub mod bare;
        pub type System = bare::BareSystem;
    }
}

/// Operating system integration.
pub trait DebuggerSystem {
    /// Indicates whether this system supports threads.
    ///
    /// If this value is false, v5gdb will not attempt to query thread data from the system.
    const MULTITHREADED: bool;

    /// Perform system-specific initialization.
    fn initialize(target: &mut V5Target);

    // Resuming preemption is not included in this trait because it is already handled directly by
    // the restore code in `overlay.S`.

    /// Tell the system's scheduler to not preempt threads, even if interrupts are enabled.
    ///
    /// Calling this multiple times should stack so that an equal number of resumes are required.
    fn suspend_preemption();

    /// Get the ID of the current thread.
    fn current_thread() -> Tid;

    /// Check whether the given thread ID refers to a thread that exists and is currently running.
    fn thread_exists(tid: Tid) -> bool;

    /// Run the given callback for each thread.
    fn all_threads(handler: &mut dyn FnMut(Tid));

    /// Get the name of the given thread and write it to `buf`, returning how many characters were
    /// written.
    ///
    /// If `buf` is not long enough, the thread name will be truncated
    fn read_thread_name(tid: Tid, buf: &mut [u8]) -> Result<usize, SystemError>;

    /// Read all the saved registers of the given thread.
    fn read_registers(tid: Tid) -> Result<ArmRegisters, SystemError>;

    /// Set the saved registers of the given thread to the given value.
    ///
    /// # Safety
    ///
    /// The given registers must be valid in the thread's current context.
    unsafe fn write_registers(tid: Tid, registers: &ArmRegisters) -> Result<(), SystemError>;

    /// Read a single register from the thread's saved state.
    fn read_single_register(tid: Tid, id: ArmRegisterID) -> Result<SavedRegister, SystemError>;

    /// Set a single registers in the thread's saved state.
    ///
    /// # Safety
    ///
    /// The given register must be valid in the thread's current context.
    unsafe fn write_single_register(
        tid: Tid,
        id: ArmRegisterID,
        value: SavedRegister,
    ) -> Result<(), SystemError>;

    /// Handle an invocation of `monitor sys` by the user.
    fn handle_monitor_cmd<'a>(_args: impl Iterator<Item = &'a str>, out: &mut ConsoleOutput) {
        gdbstub::outputln!(out, "This system doesn't support the `sys` subcommand.");
    }
}

#[derive(Debug, Snafu, Clone, Copy)]
pub enum SystemError {
    /// No such thread id
    NoSuchTid,
}

impl<T> From<SystemError> for TargetError<T> {
    fn from(value: SystemError) -> Self {
        match value {
            SystemError::NoSuchTid => TargetError::Errno(HostIoErrno::EINVAL as u8),
        }
    }
}
