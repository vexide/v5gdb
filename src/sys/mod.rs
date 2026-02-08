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
    const MULTITHREADED: bool;

    fn initialize(target: &mut V5Target);

    // Resuming preemption is not included in this trait because it is already handled directly by
    // the restore code in `overlay.S`.

    /// Tell the system's scheduler to not preempt threads, even if interrupts are enabled.
    ///
    /// Calling this multiple times should stack so that an equal number of resumes are required.
    fn suspend_preemption();

    fn current_thread() -> Tid;
    fn thread_exists(tid: Tid) -> bool;
    fn all_threads(handler: &mut dyn FnMut(Tid));
    fn read_thread_name(tid: Tid, buf: &mut [u8]) -> Result<usize, SystemError>;

    fn read_registers(tid: Tid) -> Result<ArmRegisters, SystemError>;
    fn write_registers(registers: &ArmRegisters, tid: Tid) -> Result<(), SystemError>;
    fn read_single_register(tid: Tid, id: ArmRegisterID) -> Result<SavedRegister, SystemError>;
    fn write_single_register(
        tid: Tid,
        id: ArmRegisterID,
        value: SavedRegister,
    ) -> Result<(), SystemError>;

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
