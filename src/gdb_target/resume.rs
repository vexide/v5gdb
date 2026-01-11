use gdbstub::{
    common::Signal,
    target::ext::base::singlethread::{
        SingleThreadResume, SingleThreadSingleStep, SingleThreadSingleStepOps,
    },
};

use crate::gdb_target::V5Target;

impl SingleThreadResume for V5Target {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.resume = true;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for V5Target {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.setup_step().expect("Couldn't set up single step");
        Ok(())
    }
}
