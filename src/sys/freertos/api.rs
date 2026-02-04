#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

use core::{
    ffi::{c_char, c_ulong, c_void},
    ptr,
};

use bytemuck::Zeroable;

unsafe extern "C" {
    pub unsafe static mut pxCurrentTCB: TaskHandle_t;

    #[cfg_attr(feature = "pros", link_name = "rtos_suspend_all")]
    pub safe fn vTaskSuspendAll();
    #[cfg_attr(feature = "pros", link_name = "rtos_resume_all")]
    pub unsafe fn xTaskResumeAll() -> BaseType_t;

    #[cfg_attr(feature = "pros", link_name = "task_get_current")]
    pub safe fn xTaskGetCurrentTaskHandle() -> TaskHandle_t;
    #[cfg_attr(feature = "pros", link_name = "task_get_state")]
    pub unsafe fn eTaskGetState(xTask: TaskHandle_t) -> eTaskState;

    pub unsafe fn uxTaskGetSystemState(
        pxTaskStatusArray: *mut TaskStatus_t,
        uxArraySize: UBaseType_t,
        pulTotalRunTime: *mut c_ulong,
    ) -> UBaseType_t;

    pub unsafe fn vTaskGetInfo(
        xTask: TaskHandle_t,
        pxTaskStatus: *mut TaskStatus_t,
        xGetFreeStackSpace: BaseType_t,
        eState: eTaskState,
    );
}

#[derive(Debug, Clone, Copy, Zeroable, PartialEq, Eq)]
#[repr(C)]
pub struct TaskHandle_t(pub *mut c_void);

impl TaskHandle_t {
    pub const CURRENT: Self = Self(ptr::null_mut());
}

#[derive(Debug, Clone, Copy, Zeroable, PartialEq, Eq)]
#[repr(C)]
pub struct TaskStatus_t {
    pub xHandle: TaskHandle_t,
    pub pcTaskName: *const c_char,
    pub xTaskNumber: u32,
    pub eCurrentState: eTaskState,
    pub uxCurrentPriority: u32,
    pub uxBasePriority: u32,
    pub ulRunTimeCounter: u32,
    pub pxStackBase: StackType_t,
    pub usStackHighWaterMark: u16,
}

impl TaskStatus_t {
    /// Returns a pointer to the task's TCB.
    ///
    /// Offset zero of the TCB is guaranteed to be the saved stack pointer.
    fn tcb_ptr(&self) -> *mut u32 {
        self.xHandle.0.cast()
    }

    /// Returns this task's saved stack pointer.
    ///
    /// The task this status struct refers to must not be running or else the pointer will be
    /// invalid.
    ///
    /// # Safety
    ///
    /// The task handle stored in this struct must be valid.
    pub unsafe fn sp(&self) -> u32 {
        // The stack pointer always holds the saved task state at the end.
        let saved_sp = unsafe { *self.tcb_ptr() };
        saved_sp + size_of::<SavedTaskContext>() as u32
    }

    /// Sets this task's saved stack pointer.
    ///
    /// # Safety
    ///
    /// The stack pointer must be valid.
    ///
    /// After changing the stack pointer,
    pub unsafe fn set_sp(&self, sp: u32) {
        let saved_sp = self.tcb_ptr();
        unsafe {
            *saved_sp = sp - size_of::<SavedTaskContext>() as u32;
        }
    }

    /// Returns a pointer to this task's saved task context, which is stored on the stack.
    ///
    /// # Safety
    ///
    /// The task's stack pointer must be valid and the task controlling it must not be in the
    /// [`eTaskState::RUNNING`] state.
    pub unsafe fn saved_context(&self) -> *mut SavedTaskContext {
        unsafe { *self.tcb_ptr() as *mut SavedTaskContext }
    }
}

// Handles aren't specific to any single thread - they can be sent.
unsafe impl Send for TaskStatus_t {}

#[derive(Debug, Clone, Copy, Zeroable, PartialEq, Eq)]
#[repr(C)]
pub struct eTaskState(u32);

impl eTaskState {
    pub const RUNNING: Self = Self(0);
    pub const READY: Self = Self(1);
    pub const BLOCKED: Self = Self(2);
    pub const SUSPENDED: Self = Self(3);
    pub const DELETED: Self = Self(4);
}

#[repr(C)]
pub struct SavedTaskContext {
    /// Must be true for this layout to be valid.
    pub ulPortTaskHasFPUContext: u32,
    pub fpscr: u32,
    pub d16_d31: [u64; 16],
    pub d0_d15: [u64; 16],
    pub ulCriticalNesting: u32,
    pub gp_registers: [u32; 13],
    pub lr: u32,
    pub pc: u32,
    pub spsr: u32,
}

pub type StackType_t = u32;
pub type BaseType_t = i32;
pub type UBaseType_t = u32;

pub const pdFALSE: i32 = 0;
