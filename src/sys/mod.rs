use cfg_if::cfg_if;
use gdbstub::{
    common::Tid,
    target::{TargetError, ext::host_io::HostIoErrno},
};
use snafu::Snafu;

use crate::gdb_target::{V5Target, arch::{ArmRegisterID, ArmRegisters}, single_register_access::SavedRegister};

pub mod bare;
pub mod freertos;

cfg_if! {
    if #[cfg(feature = "freertos")] {
        pub type System = freertos::FreeRtosSystem;
    } else {
        pub type System = bare::BareSystem;
    }
}

/// Operating system integration.
pub trait DebuggerSystem {
    const MULTITHREADED: bool;

    fn initialize(target: &mut V5Target);

    fn suspend_preemption();
    unsafe fn enable_preemption();

    fn current_thread() -> Tid;
    fn thread_exists(tid: Tid) -> bool;
    fn all_threads(handler: &mut dyn FnMut(Tid));

    fn read_registers(tid: Tid) -> Result<ArmRegisters, SystemError>;
    fn write_registers(registers: &ArmRegisters, tid: Tid) -> Result<(), SystemError>;
    fn read_single_register(
        tid: Tid,
        id: ArmRegisterID,
    ) -> Result<SavedRegister, SystemError>;
    fn write_single_register(
        tid: Tid,
        id: ArmRegisterID,
        value: SavedRegister,
    ) -> Result<(), SystemError>;
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
