use gdbstub::{
    common::Tid,
    target::{
        TargetError, TargetResult,
        ext::{
            base::{
                multithread::{
                    MultiThreadBase, MultiThreadResume, MultiThreadResumeOps,
                    MultiThreadSingleStep, MultiThreadSingleStepOps,
                },
                single_register_access::{SingleRegisterAccess, SingleRegisterAccessOps},
                singlethread::SingleThreadBase,
            },
            thread_extra_info::{ThreadExtraInfo, ThreadExtraInfoOps},
        },
    },
};

use crate::{
    gdb_target::{
        V5Target,
        arch::{ArmRegisterID, ArmRegisters},
        single_register_access::SavedRegister,
    },
    sys::{DebuggerSystem, System},
};

impl MultiThreadBase for V5Target {
    #[inline(always)]
    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(Tid),
    ) -> Result<(), Self::Error> {
        System::all_threads(thread_is_active);
        Ok(())
    }

    fn is_thread_alive(&mut self, tid: Tid) -> Result<bool, Self::Error> {
        Ok(System::thread_exists(tid))
    }

    fn support_thread_extra_info(&mut self) -> Option<ThreadExtraInfoOps<'_, Self>> {
        Some(self)
    }

    fn read_registers(&mut self, regs: &mut ArmRegisters, tid: Tid) -> TargetResult<(), Self> {
        if tid == System::current_thread() {
            <Self as SingleThreadBase>::read_registers(self, regs)
        } else {
            *regs = System::read_registers(tid)?;
            Ok(())
        }
    }

    fn write_registers(&mut self, regs: &ArmRegisters, tid: Tid) -> TargetResult<(), Self> {
        if tid == System::current_thread() {
            <Self as SingleThreadBase>::write_registers(self, regs)
        } else {
            System::write_registers(regs, tid)?;
            Ok(())
        }
    }

    fn read_addrs(
        &mut self,
        start_addr: u32,
        data: &mut [u8],
        _tid: Tid,
    ) -> TargetResult<usize, Self> {
        <Self as SingleThreadBase>::read_addrs(self, start_addr, data)
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8], _tid: Tid) -> TargetResult<(), Self> {
        <Self as SingleThreadBase>::write_addrs(self, start_addr, data)
    }

    fn support_resume(&mut self) -> Option<MultiThreadResumeOps<'_, Self>> {
        Some(self)
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, Tid, Self>> {
        Some(self)
    }
}

impl MultiThreadResume for V5Target {
    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        log::debug!("Setup resume");
        // All threads use the "continue" resume action by default.
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        _tid: Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        log::debug!("Resume action - continue");
        // All threads use the "continue" resume action by default.
        Ok(())
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        log::debug!("Commit resume");
        self.resume = true;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<MultiThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl MultiThreadSingleStep for V5Target {
    fn set_resume_action_step(
        &mut self,
        tid: Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        log::warn!("Resume action for {tid:?} - current = {}", System::current_thread());
        if tid == System::current_thread() {
            log::debug!("Resume action - step");
            self.setup_step().expect("Couldn't set up single step");
            Ok(())
        } else {
            Ok(())
            // unimplemented!("Can't single step a different task");
        }
    }
}

impl SingleRegisterAccess<Tid> for V5Target {
    fn read_register(
        &mut self,
        tid: Tid,
        reg_id: ArmRegisterID,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        if tid == System::current_thread() {
            <Self as SingleRegisterAccess<()>>::read_register(self, (), reg_id, buf)
        } else {
            let reg = System::read_single_register(tid, reg_id)?;
            reg.write_to_buffer(buf);
            Ok(reg.bytes())
        }
    }

    fn write_register(
        &mut self,
        tid: Tid,
        reg_id: ArmRegisterID,
        val: &[u8],
    ) -> TargetResult<(), Self> {
        if tid == System::current_thread() {
            <Self as SingleRegisterAccess<()>>::write_register(self, (), reg_id, val)
        } else {
            let reg = SavedRegister::from_le_bytes(val).ok_or(TargetError::NonFatal)?;
            System::write_single_register(tid, reg_id, reg)?;
            Ok(())
        }
    }
}

impl ThreadExtraInfo for V5Target {
    fn thread_extra_info(&self, tid: Tid, buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(System::read_thread_name(tid, buf).unwrap_or(0))
    }
}
