.text
.arm

.set SIZEOF_DebugEventContext, 0x148
.set MODE_ABT, 0b10111
.set MODE_SYS, 0b11111
.if SIZEOF_DebugEventContext % 8 != 0
    .error "expect multiple of 8 for copy"
.endif
.ifdef PROS
    .set xTaskResumeAll, rtos_resume_all
.endif

@ An overlay vector table that intercepts debug exception (breakpoints and watchpoints) but
@ otherwise delegates exceptions to an existing vector table.
.global v5gdb_debugger_vector_table
.type v5gdb_debugger_vector_table, %object
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
    b v5gdb_irq_handler         @ IRQ interrupt
    ldr pc, original_fiq_addr   @ FIQ interrupt

@ This is an array of pointers to each offset of the base vector table.
@ When configured at runtime, each entry is (original_vector_table + <OFFSET> * 4).
@ Since we pre-calculate the offsets, evaluating (*original_vector_addresses) + N is the same as
@ evaluating *(original_vector_addresses + N). The latter can be done in just one instruction, which
@ allows us to use it in the overlay vector table.
.global v5gdb_original_vector_addresses
.type v5gdb_original_vector_addresses, %object
v5gdb_original_vector_addresses:
    original_reset_addr: .word 0
    original_udf_addr: .word 0
    original_svc_addr: .word 0
    original_prefetch_abt_addr: .word 0
    original_data_abt_addr: .word 0
    .word 0
    original_irq_addr: .word 0
    original_fiq_addr: .word 0

@ Intercept IRQ exceptions so can periodically poll the debugger without user code cooperation.
@
@ We intentionally avoid identifying the interrupt source here (by reading ICCIAR) since that would
@ ACK the interrupt which isn't our responsibility. Instead the `v5gdb_irq_poll` is responsible
@ for ratelimiting itself to avoid taking up too much time from user code.
.type v5gdb_irq_handler, %function
v5gdb_irq_handler:
    @ Save caller-saved registers and the exception return address since `bl` will clobber `lr`.
    push {r0-r3, r12, lr}
    vpush {d16-d31}
    vpush {d0-d7}
    vmrs r0, fpscr
    push {r0, r1} @ r1 is just a bogus register for stack alignment
    blx v5gdb_irq_poll
    pop {r0, r1}
    vmsr fpscr, r0
    vpop {d0-d7}
    vpop {d16-d31}
    pop {r0-r3, r12, lr}
    @ Chain to the original IRQ handler, which will perform the actual exception return.
    ldr pc, original_irq_addr

@ These vector table handlers will fall through for normal aborts, but debug events are caught and
@ redirected to the Rust breakpoint handling logic.
.type prefetch_abort_handler, %function
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
.type data_abort_handler, %function
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
.type catch_debug_event, %function
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
    cpsid if

    @ Re-enable the scheduler if necessary. When configured for FreeRTOS, the debug monitor disables
    @ the task scheduler to prevent other tasks from running while the debugger is editing their
    @ state.
    @ Note that this happens before we restore the general purpose registers, so we can clobber them
    @ as desired.
.ifdef FREERTOS
    @ Check the return value from `v5gdb_handle_debug_event`, which indicates whether to resume the
    @ scheduler.
    @ Motivation: This will be false in the case of a single step, since the debugger's immunity
    @ from hardware breakpoints is only because it runs in Abort Mode. Resuming the scheduler
    @ involves switching to System Mode, which forfeits that power, firing the single step inside of
    @ the abort handler itself. Obviously, single steps are supposed to step *user* code, so this
    @ would be suboptimal.
    cmp r0, #0
    beq .Lrestore_without_rtos_resume

    @ We can't yield yet since we still have a bunch of state on the abort stack; if we did, another
    @ task might hit its own breakpoint, reenter abort mode, and write its own state. This could
    @ lead to us then popping the other task's state off the stack. Resolve this by copying
    @ everything to the user-mode stack.
    mov r0, sp
    add r1, sp, #SIZEOF_DebugEventContext
    cps #MODE_SYS
    bic sp, sp, #0b111 @ Align the user's stack.
.Lcopy_to_user_stack_loop:
    ldmdb r1!, {r3,r4}
    push {r3,r4}
    cmp r0, r1
    bne .Lcopy_to_user_stack_loop

    @ Now that we've copied everything over, free the debug event from the abort stack.
    cps #MODE_ABT
    add sp, #SIZEOF_DebugEventContext

    @ Resume tasks - this might yield. This requires us to be in System mode.
    @ This is AAPCS so some regs may be clobbered; OK since everything is on the stack.
    cps #MODE_SYS
    blx xTaskResumeAll @ fn() -> u32

    @ Turn interrupts back off after the context switch and go back to abort mode so we're allowed
    @ to do an exception return.
    mov r0, sp
    cpsid if, #MODE_ABT
    b v5gdb_restore_user_state
.endif
.Lrestore_without_rtos_resume:
    @ Clean up our stack, then restore.
    mov r0, sp
    add sp, #SIZEOF_DebugEventContext
    @ [Fall through to v5gdb_restore_user_state]

@ Applies a DebugEventContext; does not push/pop from the stack.
@ Params: r0: *const DebugEventContext
@ Prerequisites: CPU in Abort mode, interrupts off.
.type v5gdb_restore_user_state, %function
v5gdb_restore_user_state:
    @ Prepare CPU mode and special regs for the user.
    ldm r0, {r1,sp,lr}^
    add r0, #12
    msr spsr, r1

    @ Floating-point/simd registers
    ldm r0!, {r1}
    vmsr fpscr, r1
    vldm r0!, {d0-d15}
    vldm r0!, {d16-d31}

    @ User regs & PC. This applies the spsr we set earlier.
    ldm r0, {r0-r12,pc}^
