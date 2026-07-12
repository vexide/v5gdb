use gdbstub::{
    common::{Signal, Tid},
    target::ext::base::{multithread::{MultiThreadResume, MultiThreadSingleStep, MultiThreadSingleStepOps}, singlethread::{
        SingleThreadResume, SingleThreadSingleStep, SingleThreadSingleStepOps,
    }},
};

use crate::{gdb_target::{MonitorStatus, V5Target}, sys::{DebuggerSystem, System}};

impl SingleThreadResume for V5Target {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.monitor_status = MonitorStatus::ResumingProgram;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl MultiThreadResume for V5Target {
    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        log::info!("Setup resume");
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
        log::info!("Commit resume");
        self.monitor_status = MonitorStatus::ResumingProgram;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<MultiThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for V5Target {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.request_single_step().expect("Couldn't set up single step");
        self.monitor_status = MonitorStatus::ResumingProgram;
        Ok(())
    }
}

impl MultiThreadSingleStep for V5Target {
    fn set_resume_action_step(
        &mut self,
        tid: Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        log::info!(
            "Resume action STEP for tid {tid:?} (current = {})",
            System::current_thread()
        );
        if tid == System::current_thread() {
            self.request_single_step().expect("Couldn't set up single step");
            Ok(())
        } else {
            unimplemented!("Can't single step a different thread");
        }
    }
}
