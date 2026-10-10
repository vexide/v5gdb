use gdbstub::{
    common::Tid,
    target::{
        TargetResult,
        ext::{
            base::{
                multithread::{MultiThreadBase, MultiThreadResumeOps},
                single_register_access::SingleRegisterAccessOps,
                singlethread::SingleThreadBase,
            },
            thread_extra_info::{ThreadExtraInfo, ThreadExtraInfoOps},
        },
    },
};

use crate::{
    exceptions::DebugEventContext, gdb_target::V5Target, sys::{DebuggerSystem, System}
};

impl MultiThreadBase for V5Target {
    #[inline(always)]
    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(Tid),
    ) -> Result<(), Self::Error> {
        // gdbstub considers the first thread we report to be the current thread.
        let current = System::current_thread();
        thread_is_active(current);
        System::all_threads(&mut |tid| {
            if tid != current {
                thread_is_active(tid);
            }
        });
        Ok(())
    }

    fn is_thread_alive(&mut self, tid: Tid) -> Result<bool, Self::Error> {
        Ok(System::thread_exists(tid))
    }

    fn support_thread_extra_info(&mut self) -> Option<ThreadExtraInfoOps<'_, Self>> {
        Some(self)
    }

    fn read_registers(&mut self, regs: &mut DebugEventContext, tid: Tid) -> TargetResult<(), Self> {
        if tid == System::current_thread() {
            <Self as SingleThreadBase>::read_registers(self, regs)
        } else {
            *regs = System::read_registers(tid)?;
            Ok(())
        }
    }

    fn write_registers(&mut self, regs: &DebugEventContext, tid: Tid) -> TargetResult<(), Self> {
        if tid == System::current_thread() {
            <Self as SingleThreadBase>::write_registers(self, regs)
        } else {
            // SAFETY: We trust that GDB will not corrupt system state.
            unsafe {
                System::write_registers(tid, regs)?;
            }
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

impl ThreadExtraInfo for V5Target {
    fn thread_extra_info(&self, tid: Tid, buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(System::read_thread_name(tid, buf).unwrap_or(0))
    }
}
