//! Bindings and helpers for FreeRTOS APIs.
//!
//! # Working with tasks
//!
//! Start by getting a task's [status](TaskStatus_t) with [`vTaskGetInfo`]. After doing so, you
//! will be able to access its details via the members of the struct or via the helper functions
//! on the task status struct.
//!
//! A notable convention used in this module is the separation of a tasks's *logical stack pointer*
//! and the actual saved stack pointer in its TCB. When a task is switched out, FreeRTOS
//! automatically pushes all its registers to the stack. That operation is later undone when the
//! task is continued. Since the task never actually sees its registers on the stack, v5gdb
//! considers its *logical stack* to have that saved state popped off. Thus, querying the stack
//! pointer using the functions in this module will give a value slightly above (or, "to the right")
//! of the actual saved stack pointer to match what the task sees when it's running.

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
    fn tcb_ptr(&self) -> *mut TaskControlBlock {
        self.xHandle.0.cast()
    }

    /// Returns this task's logical saved stack pointer.
    ///
    /// The task this status struct refers to must not be running or else the pointer will be
    /// invalid. The task's saved context is always placed just after the end of the
    ///
    /// # Safety
    ///
    /// The task handle stored in this struct must be valid.
    pub unsafe fn sp(&self) -> u32 {
        // The first item on a switched-out task's stack is always its saved context, but the
        // "logical" stack pointer has that popped off because that's just a FreeRTOS implementation
        // detail.
        let stack_ptr = unsafe { (*self.tcb_ptr()).stack_ptr };
        stack_ptr + size_of::<SavedTaskContextFPU>() as u32
    }

    /// Sets this task's logical saved stack pointer.
    ///
    /// Changing the stack pointer will also change the address returned by [`Self::saved_context`].
    /// If `update_ctx` is set, the task's old saved context will be copied to the new stack.
    ///
    /// # Safety
    ///
    /// The stack pointer must be valid and the task must not be running.
    pub unsafe fn set_sp(&self, mut sp: u32, update_ctx: bool) {
        // Add room for the task's saved context.
        sp -= size_of::<SavedTaskContextFPU>() as u32;

        unsafe {
            let old_ctx = self.saved_context();

            (*self.tcb_ptr()).stack_ptr = sp;
            if update_ctx {
                ptr::copy(old_ctx, sp as *mut SavedTaskContextFPU, 1);
            }
        }
    }

    /// Returns a pointer to this task's saved task context, which is stored on the stack.
    ///
    /// # Panics
    ///
    /// Panics if the task doesn't have a saved FPU context.
    ///
    /// # Safety
    ///
    /// The task's stack pointer must be valid and the task controlling it must not be in the
    /// [`eTaskState::RUNNING`] state.
    pub unsafe fn saved_context(&self) -> *mut SavedTaskContextFPU {
        unsafe {
            let stack_ptr = (*self.tcb_ptr()).stack_ptr;
            // The saved context is always pushed onto the stack when a task is switched out.
            let ctx = stack_ptr as *mut SavedTaskContextFPU;

            let is_fpu = (*ctx).ulPortTaskHasFPUContext != 0;
            if is_fpu {
                ctx
            } else {
                // The layout of the saved context is different when there's no fpu data.
                unimplemented!("tasks without fpu contexts")
            }
        }
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
struct TaskControlBlock {
    stack_ptr: u32,
    // ... The rest of the layout is not considered stable.
}

#[repr(C)]
pub struct SavedTaskContextFPU {
    /// Must be true for this layout to be valid.
    ulPortTaskHasFPUContext: u32,
    pub fpu: FPUContext,
    pub base: BaseContext,
}

impl SavedTaskContextFPU {
    pub fn new(fpu: FPUContext, base: BaseContext) -> Self {
        Self {
            ulPortTaskHasFPUContext: 1,
            fpu,
            base,
        }
    }
}

#[repr(C)]
pub struct FPUContext {
    pub fpscr: u32,
    pub d16_d31: [u64; 16],
    pub d0_d15: [u64; 16],
}

#[repr(C)]
pub struct BaseContext {
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
