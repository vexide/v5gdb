use gdbstub::common::Tid;

use crate::{
    gdb_target::{
        V5Target,
        arch::{ArmRegisterID, ArmRegisters},
        single_register_access::SavedRegister,
    },
    sys::{DebuggerSystem, SystemError},
};

pub struct BareSystem {}

impl DebuggerSystem for BareSystem {
    const MULTITHREADED: bool = false;

    fn initialize(_target: &mut V5Target) {}

    #[inline(always)]
    fn suspend_preemption() {}

    #[inline(always)]
    fn current_thread() -> Tid {
        Tid::new(1).unwrap()
    }

    #[inline(always)]
    fn thread_exists(tid: Tid) -> bool {
        tid == Self::current_thread()
    }

    #[inline(always)]
    fn all_threads(handler: &mut dyn FnMut(Tid)) {
        handler(Self::current_thread())
    }

    #[inline(always)]
    fn read_registers(_tid: Tid) -> Result<ArmRegisters, SystemError> {
        Err(SystemError::NoSuchTid)
    }

    #[inline(always)]
    unsafe fn write_registers(_tid: Tid, _registers: &ArmRegisters) -> Result<(), SystemError> {
        Err(SystemError::NoSuchTid)
    }

    #[inline(always)]
    fn read_single_register(_tid: Tid, _id: ArmRegisterID) -> Result<SavedRegister, SystemError> {
        Err(SystemError::NoSuchTid)
    }

    #[inline(always)]
    unsafe fn write_single_register(
        _tid: Tid,
        _id: ArmRegisterID,
        _value: SavedRegister,
    ) -> Result<(), SystemError> {
        Err(SystemError::NoSuchTid)
    }

    fn read_thread_name(_tid: Tid, _buf: &mut [u8]) -> Result<usize, SystemError> {
        Err(SystemError::NoSuchTid)
    }
}
