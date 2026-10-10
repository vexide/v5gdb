use gdbstub::{
    common::{Signal, Tid},
    target::ext::base::{
        multithread::{
            MultiThreadResume, MultiThreadSchedulerLocking, MultiThreadSchedulerLockingOps,
            MultiThreadSingleStep, MultiThreadSingleStepOps,
        },
        singlethread::{SingleThreadResume, SingleThreadSingleStep, SingleThreadSingleStepOps},
    },
};

use crate::{
    gdb_target::{MonitorStatus, StopReason, V5Target},
    logging,
    sys::{DebuggerSystem, System},
};

#[derive(Debug, Default, PartialEq, Clone, Copy)]
enum ResumeKind {
    #[default]
    Unspecified,
    Continue,
    Step,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResumeActions {
    current_thread: ResumeKind,
    resuming_some_other_threads: bool,
    /// If specified, threads with unspecified resume actions must not be resumed.
    only_continue_if_specified: bool,
    invalid: bool,
}

impl SingleThreadResume for V5Target {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.resume = ResumeActions {
            current_thread: ResumeKind::Continue,
            ..Default::default()
        };
        MultiThreadResume::resume(self)
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for V5Target {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.resume = ResumeActions {
            current_thread: ResumeKind::Step,
            ..Default::default()
        };
        MultiThreadResume::resume(self)
    }
}

impl MultiThreadResume for V5Target {
    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        logging::trace!("Clearing resume actions");
        self.resume = ResumeActions::default();
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        tid: Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        logging::debug!("Resume action for {tid}: continue");
        if tid == System::current_thread() {
            self.resume.current_thread = ResumeKind::Continue;
        } else {
            self.resume.resuming_some_other_threads = true;
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        if !self.resume.invalid {
            self.try_resume();
        }

        if self.resume.invalid {
            logging::error!("Failed to resume the program");
            self.stop_reason = StopReason::ResumeFailed;
        }

        Ok(())
    }

    fn support_single_step(&mut self) -> Option<MultiThreadSingleStepOps<'_, Self>> {
        Some(self)
    }

    fn support_scheduler_locking(&mut self) -> Option<MultiThreadSchedulerLockingOps<'_, Self>> {
        Some(self)
    }
}

impl MultiThreadSingleStep for V5Target {
    fn set_resume_action_step(
        &mut self,
        tid: Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        logging::debug!(
            "Resume action: step thread {tid} (current thread is {})",
            System::current_thread()
        );
        if tid == System::current_thread() {
            self.resume.current_thread = ResumeKind::Step;
        } else {
            logging::error!("only the current thread may be single stepped");
            self.resume.invalid = true;
        }
        Ok(())
    }
}

impl MultiThreadSchedulerLocking for V5Target {
    fn set_resume_action_scheduler_lock(&mut self) -> Result<(), Self::Error> {
        self.resume.only_continue_if_specified = true;
        Ok(())
    }
}

impl V5Target {
    fn try_resume(&mut self) {
        logging::trace!("Committing resume actions");

        let mut lock_scheduler = false;

        if self.resume.only_continue_if_specified {
            // We don't have a good way to resume some threads but not others (except for the
            // current thread), so for now we'll just bail out.
            if self.resume.resuming_some_other_threads {
                logging::error!("resuming some inactive threads but not others is not supported");
                self.resume.invalid = true;
                return;
            }
            if self.resume.current_thread == ResumeKind::Unspecified {
                // Nothing to resume.
                return;
            }

            // In this case, we're resuming the current thread but nothing else, so pause the
            // scheduler to keep the CPU on the same thread.
            lock_scheduler = true;
        }

        if self.resume.current_thread == ResumeKind::Step {
            let res = self.request_single_step();
            if let Err(err) = res {
                logging::error!("could not create single-step breakpoint: {err}");
                self.resume.invalid = true;
                return;
            }
        }

        self.force_scheduler_suspend = lock_scheduler;
        self.monitor_status = MonitorStatus::ResumingProgram;
    }
}
