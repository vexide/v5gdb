.text
.arm

@ An overlay vector table that intercepts debug exception (breakpoints and watchpoints) but
@ otherwise delegates exceptions to an existing vector table.
.global v5gdb_debugger_vector_table
.align 5
v5gdb_debugger_vector_table:
    @ To fall through to an original vector table entry, we jump to the offset of the
    @ original_vector_addresses array which stores the address of the entry we need.
    ldr pc, original_reset_addr @ Reset
    ldr pc, original_udf_addr   @ Undefined instruction
    ldr pc, original_svc_addr   @ Supervisor Call
    b prefetch_abort_handler    @ Prefetch abort
    b data_abort_handler        @ Data abort
    nop                         @ Not used
    ldr pc, original_irq_addr   @ IRQ interrupt
    ldr pc, original_fiq_addr   @ FIQ interrupt

@ This is an array of pointers to each offset of the base vector table.
@ When configured at runtime, each entry is (original_vector_table + <OFFSET> * 4).
@ Since we pre-calculate the offsets, evaluating (*original_vector_addresses) + N is the same as
@ evaluating *(original_vector_addresses + N). The latter can be done in just one instruction, which
@ allows us to use it in the overlay vector table.
.global v5gdb_original_vector_addresses
v5gdb_original_vector_addresses:
    original_reset_addr: .word 0
    original_udf_addr: .word 0
    original_svc_addr: .word 0
    original_prefetch_abt_addr: .word 0
    original_data_abt_addr: .word 0
    .word 0
    original_irq_addr: .word 0
    original_fiq_addr: .word 0

@ These vector table handlers will fall through for normal aborts, but debug events are caught and
@ redirected to the Rust breakpoint handling logic.
prefetch_abort_handler:
    dsb @ Workaround for Cortex-A9 erratum (id 775420)

    @ First, update the processor's condition flag to indicate whether the exception is a debug
    @ event. Importantly (!), no general-purpose registers are clobbered, allowing the framework's
    @ primary abort handler to capture and show the user the values of these registers from the time
    @ of the abort.
    push {r0}
    mrc p15, 0, r0, c5, c0, 1 @ r0 <- IFSR
    and r0, #0b1111 @ Check the FS[3:0](Fault status) bits.
    @ This comparison does overwrite the comparison flags in APSR, but the ones from before the
    @ abort are stored in SPSR anyways so it's not an issue.
    cmp r0, #0b00010 @ FS == 0b00010(Debug event)?
    pop {r0}

    @ Debug event -> catch it.
    @ Not a debug event -> fall through to original prefetch abort handler.
    ldrne pc, original_prefetch_abt_addr
    sub lr, #4  @ Offset LR to match the preferred return address (see B1.9.7 in ARMv7-A manual).
    b catch_debug_event

@ For watchpoints.
data_abort_handler:
    dsb

    push {r0}
    mrc p15, 0, r0, c5, c0, 0 @ r0 <- DFSR
    and r0, #0b1111 @ Check the FS[3:0](Fault status) bits.
    cmp r0, #0b00010 @ FS == 0b00010(Debug event)?
    pop {r0}

    ldrne pc, original_data_abt_addr
    sub lr, #8  @ Offset LR to match the preferred return address (see B1.9.8 in ARMv7-A manual).
    b catch_debug_event

@ Saves the current program state after a breakpoint or watchpoint and switches into the debug
@ monitor for inspection and modification.
@ The stack should be aligned to 8 bytes when calling this routine.
catch_debug_event:
    @ -- Save --
    @ Create a DebugEventContext struct on the stack.

    @ Store general purpose registers for debugging and exception return
    push {r0-r12,lr}

    @ Save floating-point/simd registers (they are the same).
    @ VPUSH has a maximum of 16 registers, so we do it in groups.
    vpush {d16-d31}
    vpush {d0-d15}
    vmrs r0, fpscr
    push {r0}

    @ Store original user/system-mode stack pointer and link register for debug
    stmdb sp, {sp,lr}^
    @ Adjust our sp, since we can't use writeback on STM (User registers)
    sub sp, sp, #8

    @ Store the caller's program status register
    mrs r0, spsr
    push {r0}

    @ -- Handle exception --

    @ Pass it to our handler using the C ABI, fn(*mut DebugEventContext) -> bool:
    mov r0, sp              @ Set param 0
    blx v5gdb_handle_debug_event  @ Actually call the function now (stack is already aligned correctly)

    @ -- Restore --

    @ Prevent ourselves from getting preempted while messing with task state - this is especially
    @ important on systems with a preemptive scheduler (i.e. FreeRTOS).
    cpsid i

    @ Saved program status
    pop {r1}
    msr spsr, r1

    @ Pop user/system mode banked registers back - SP, LR
    ldm sp, {sp,lr}^
    add sp, sp, #8

    @ Re-enable the scheduler if necessary. When configured for FreeRTOS, the debug monitor disables
    @ the task scheduler to prevent other tasks from running while the debugger is editing their
    @ state.
    @ Note that this happens before we restore the general purpose registers, so we can clobber them
    @ as desired.
.ifdef FREERTOS
    .ifdef PROS
        .set xTaskResumeAll, rtos_resume_all
    .endif

    @ Check the return value from `v5gdb_handle_debug_event`, which indicates whether to resume the
    @ scheduler.
    @ Motivation: This will be false in the case of a single step, since the debugger's immunity
    @ from hardware breakpoints is only because it runs in Abort Mode. Resuming the scheduler
    @ involves switching to System Mode, which forfeits that power, firing the single step inside of
    @ the abort handler itself. Obviously, single steps are supposed to step *user* code, so this
    @ would be suboptimal.
    cbz r0, .Lafter_freertos_scheduler_unpause

    @ Switch to System Mode -- resuming the scheduler can yield, which is only sound if the CPU is
    @ in System Mode.
    @ NB. This will set the current stack to the system mode stack, which has already been updated
    @ (above) to its new location. LR and SPSR are also banked.
    cps #0b11111

    @ (Save old stack ptr, then...) Align stack to 8 bytes for the AAPCS call
    mov r0, sp
    bic sp, sp, #0b111

    @ Resume tasks - this might yield.
    push {r0,lr}
    blx xTaskResumeAll @ fn() -> u32
    pop {r0,lr}

    @ Restore stack pointer to previous value
    mov sp, r0

    @ Switch back to abort mode so that we restore the rest of the saved registers from the task.
    @ This is also necessary so that the current SPSR is the one we popped into (SPSR_abt).
    cps #0b10111
.Lafter_freertos_scheduler_unpause:
.endif

    @ Floating-point/simd registers
    pop {r0}
    vmsr fpscr, r0
    vpop {d0-d15}
    vpop {d16-d31}

    @ And... perform an exception return by loading the old LR_abt into PC.
    @ Note that we've already offset LR as required to point to the correct exception return
    @ address.
    ldm sp!, {r0-r12, pc}^
