use core::{ffi::CStr, mem::MaybeUninit, ptr};

use gdbstub::{common::Tid, target::ext::monitor_cmd::ConsoleOutput};
use owo_colors::{OwoColorize, Style};
use spin::Mutex;

use self::api::{TaskHandle_t, TaskStatus_t, UBaseType_t, eTaskState, pdFALSE};
use crate::{
    gdb_target::{
        V5Target,
        arch::{ArmRegisterID, ArmRegisters},
        single_register_access::SavedRegister,
    },
    sys::{DebuggerSystem, SystemError, freertos::api::SavedTaskContext},
};

mod api;

pub struct FreeRtosSystem {}

impl DebuggerSystem for FreeRtosSystem {
    const MULTITHREADED: bool = true;

    fn initialize(_target: &mut V5Target) {}

    fn suspend_preemption() {
        api::vTaskSuspendAll();
    }

    fn all_threads(handler: &mut dyn FnMut(Tid)) {
        for task in scan_tasks() {
            if task.eCurrentState != eTaskState::DELETED {
                handler(Tid::new(task.xTaskNumber as usize).expect("Thread IDs should be nonzero"));
            }
        }
    }

    fn current_thread() -> Tid {
        let mut status = MaybeUninit::uninit();
        unsafe {
            api::vTaskGetInfo(
                TaskHandle_t::CURRENT,
                status.as_mut_ptr(),
                pdFALSE,
                eTaskState::RUNNING,
            );
        }
        let status = unsafe { status.assume_init() };
        Tid::new(status.xTaskNumber as usize).expect("Thread IDs should be nonzero")
    }

    fn thread_exists(tid: Tid) -> bool {
        scan_tasks().any(|task| task.xTaskNumber as usize == tid.get())
    }

    fn read_registers(tid: Tid) -> Result<ArmRegisters, SystemError> {
        let task = lookup_saved_task(tid)?;

        // SAFETY: The task is valid and not running.
        let ctx = unsafe {
            let ptr = task.saved_context();
            assert!(
                unsafe { (*ptr).ulPortTaskHasFPUContext != 0 },
                "Only tasks with FP contexts are supported"
            );
            ptr.read()
        };

        Ok(ArmRegisters {
            r: ctx.gp_registers,
            sp: unsafe { task.sp() },
            lr: ctx.lr,
            pc: ctx.pc,
            cpsr: ctx.spsr,
            d: [ctx.d0_d15, ctx.d16_d31].as_flattened().try_into().unwrap(),
            fpscr: ctx.fpscr,
        })
    }

    fn write_registers(registers: &ArmRegisters, tid: Tid) -> Result<(), SystemError> {
        let task = lookup_saved_task(tid)?;

        unsafe {
            task.set_sp(registers.sp);

            // SAFETY: The task is valid and not running.
            let ctx = task.saved_context();
            assert!(
                unsafe { (*ctx).ulPortTaskHasFPUContext != 0 },
                "Only tasks with FP contexts are supported"
            );

            ctx.write(SavedTaskContext {
                ulPortTaskHasFPUContext: 1,
                fpscr: registers.fpscr,
                d16_d31: *registers.d[16..=31].as_array().unwrap(),
                d0_d15: *registers.d[0..=15].as_array().unwrap(),
                ulCriticalNesting: (*ctx).ulCriticalNesting,
                gp_registers: registers.r,
                lr: registers.lr,
                pc: registers.pc,
                spsr: registers.cpsr,
            })
        };

        Ok(())
    }

    fn read_single_register(tid: Tid, id: ArmRegisterID) -> Result<SavedRegister, SystemError> {
        let task = lookup_saved_task(tid)?;

        if id == ArmRegisterID::Sp {
            return Ok(SavedRegister::U32(unsafe { task.sp() }));
        }

        let ctx = unsafe { task.saved_context() };
        assert!(
            unsafe { (*ctx).ulPortTaskHasFPUContext != 0 },
            "Only tasks with FP contexts are supported"
        );

        unsafe {
            Ok(match id {
                ArmRegisterID::Gpr(i) => SavedRegister::U32((*ctx).gp_registers[i as usize]),
                ArmRegisterID::Lr => SavedRegister::U32((*ctx).lr),
                ArmRegisterID::Pc => SavedRegister::U32((*ctx).pc),
                ArmRegisterID::Cpsr => SavedRegister::U32((*ctx).spsr),
                ArmRegisterID::Fpr(i) => SavedRegister::U64({
                    if let Some(i) = i.checked_sub(16) {
                        (*ctx).d16_d31[i as usize]
                    } else {
                        (*ctx).d0_d15[i as usize]
                    }
                }),
                ArmRegisterID::Fpscr => SavedRegister::U32((*ctx).fpscr),
                ArmRegisterID::Sp => unreachable!(),
            })
        }
    }

    fn write_single_register(
        tid: Tid,
        id: ArmRegisterID,
        value: SavedRegister,
    ) -> Result<(), SystemError> {
        let task = lookup_saved_task(tid)?;

        if id == ArmRegisterID::Sp {
            unsafe {
                // The task's registers are saved just below its stack. For state restoration to
                // work, they have to remain in this predictable spot next to the new stack.
                let ptr = task.saved_context();
                assert!(
                    unsafe { (*ptr).ulPortTaskHasFPUContext != 0 },
                    "Only tasks with FP contexts are supported"
                );
                let old_ctx = ptr.read();

                task.set_sp(value.unwrap_u32());
                task.saved_context().write(old_ctx);
            }
        }

        let ctx = unsafe { task.saved_context() };

        unsafe {
            match id {
                ArmRegisterID::Gpr(i) => (*ctx).gp_registers[i as usize] = value.unwrap_u32(),
                ArmRegisterID::Lr => (*ctx).lr = value.unwrap_u32(),
                ArmRegisterID::Pc => (*ctx).pc = value.unwrap_u32(),
                ArmRegisterID::Cpsr => (*ctx).spsr = value.unwrap_u32(),
                ArmRegisterID::Fpr(i) => {
                    if let Some(i) = i.checked_sub(16) {
                        (*ctx).d16_d31[i as usize] = value.unwrap_u64();
                    } else {
                        (*ctx).d0_d15[i as usize] = value.unwrap_u64();
                    }
                }
                ArmRegisterID::Fpscr => (*ctx).fpscr = value.unwrap_u32(),
                ArmRegisterID::Sp => unreachable!(),
            }

            Ok(())
        }
    }

    fn read_thread_name(tid: Tid, buf: &mut [u8]) -> Result<usize, SystemError> {
        let task = scan_tasks()
            .find(|task| task.xTaskNumber as usize == tid.get())
            .ok_or(SystemError::NoSuchTid)?;

        let name = unsafe { CStr::from_ptr(task.pcTaskName) };
        let name_bytes = name.to_bytes();

        let copy_len = name_bytes.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

        Ok(copy_len)
    }

    fn handle_monitor_cmd<'a>(mut args: impl Iterator<Item = &'a str>, out: &mut ConsoleOutput) {
        let cmd = args.next().unwrap_or("?");

        match cmd {
            "t" | "task" | "tasks" => {
                gdbstub::outputln!(
                    out,
                    "  {:<3} {:<32} {:<10} {:<10} {:<10}",
                    "Id",
                    "Name",
                    "State",
                    "Priority",
                    "Stack Remaining"
                );

                let current = unsafe { api::pxCurrentTCB };

                for task in scan_tasks() {
                    let marker = if task.xHandle == current { "*" } else { " " };
                    let id = task.xTaskNumber;
                    let name = unsafe { CStr::from_ptr(task.pcTaskName) };

                    let state = match task.eCurrentState {
                        eTaskState::BLOCKED => "Blocked".style(Style::new().yellow()),
                        eTaskState::DELETED => "Deleted".style(Style::new().red()),
                        eTaskState::READY => "Ready".style(Style::new().green()),
                        eTaskState::RUNNING => "Running".style(Style::new().green().bold()),
                        eTaskState::SUSPENDED => "Suspend".style(Style::new().magenta()),
                        _ => "Unknown".style(Style::new().bright_black()),
                    };

                    let priority = task.uxCurrentPriority;
                    let stack_rem = task.usStackHighWaterMark;

                    gdbstub::outputln!(
                        out,
                        "{marker} {id:<3} {name:<32} {state:<10} {priority:<10} {stack_rem:<10}",
                        name = name.to_str().unwrap_or("<Invalid UTF-8>"),
                    );
                }
            }
            "?" | "help" | "h" => {
                gdbstub::outputln!(out, "v5gdb is configured for FreeRTOS{FREERTOS_FLAVOR}.");
                gdbstub::outputln!(out, "{MONITOR_HELP}");
            }
            _ => {
                gdbstub::outputln!(
                    out,
                    "Unknown command. See 'monitor sys help' for more info."
                );
            }
        }
    }
}

const FREERTOS_FLAVOR: &str = if cfg!(feature = "pros") {
    " (PROS flavor)"
} else {
    ""
};
const MONITOR_HELP: &str = r#"FreeRTOS-specific commands:
    sys help                            Show this help message.
    sys tasks                           Lists the following details about each running task:
                                        * Id: The unique task ID number.
                                        * Name: Task name
                                        * State: The scheduling state of the task.
                                        * Priority: The scheduling priority of the task.
                                        * Stack Remaining: The amount of bytes in the task's stack
                                        which have never been used.
"#;

fn scan_tasks() -> impl Iterator<Item = TaskStatus_t> {
    static TASK_ARRAY: Mutex<[MaybeUninit<TaskStatus_t>; 128]> =
        Mutex::new([MaybeUninit::uninit(); _]);

    let mut task_array = TASK_ARRAY.lock();
    let num_tasks = unsafe {
        api::uxTaskGetSystemState(
            task_array.as_mut_ptr().cast(),
            task_array.len() as UBaseType_t,
            ptr::null_mut(),
        ) as usize
    };

    (0..num_tasks).map(move |idx| unsafe { task_array.get_unchecked(idx).assume_init() })
}

fn lookup_saved_task(tid: Tid) -> Result<TaskStatus_t, SystemError> {
    let task = scan_tasks()
        .find(|task| task.xTaskNumber as usize == tid.get())
        .ok_or(SystemError::NoSuchTid)?;

    assert!(task.eCurrentState != eTaskState::RUNNING);

    Ok(task)
}
