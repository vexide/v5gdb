use crate::cpu::ProgramStatus;

/// The saved state of a program from before an exception.
///
/// Note that updating these fields will cause the exception handler to apply the changes to the CPU
/// if/when the current exception handler returns.
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct DebugEventContext {
    /// The saved program status register (spsr) from before the exception.
    pub spsr: ProgramStatus,
    /// The stack pointer from before the exception.
    pub stack_pointer: u32,
    /// The link register from before the exception.
    pub link_register: u32,

    /// Floating point status and control register.
    pub fpscr: u32,
    /// Floating point registers d0 through d31
    pub vfp_registers: [u64; 32],

    /// Registers r0 through r12
    pub registers: [u32; 13],

    /// The address at which the abort occurred.
    ///
    /// This is calculated using the Link Register (`lr`) at abort time, which is set to this
    /// address plus an offset when an exception occurs.
    pub program_counter: u32,
}

#[cfg(target_arch = "arm")]
pub(crate) mod arm {
    use core::{
        arch::asm,
        array,
        ffi::c_void,
        mem::MaybeUninit,
        sync::atomic::{AtomicBool, Ordering},
    };

    use aarch32_cpu::asm::dsb;

    use crate::{
        DEBUGGER,
        cpu::{exception::VectorBaseAddressRegister, instruction::Instruction},
        exceptions::DebugEventContext,
        sys::{DebuggerSystem, System},
    };

    core::arch::global_asm!(
        #[cfg(feature = "freertos")]
        ".set FREERTOS, 1",
        #[cfg(feature = "pros")]
        ".set PROS, 1",
        include_str!("./overlay.S"),
        options(raw),
    );

    const ABORT_STACK_SIZE: usize = 0x8000; // 32KB

    #[repr(C, align(8))]
    struct AbortStack(MaybeUninit<[u8; const { ABORT_STACK_SIZE }]>);
    static mut ABORT_STACK: AbortStack = AbortStack(MaybeUninit::uninit());

    /// Handles a debug event.
    ///
    /// This function is called from the abort handler routines in `overlay.S` once they've been
    /// activated with [`install_vectors`].
    ///
    /// # Safety
    ///
    /// Must be passed a debug event context that's valid for reads and writes and lives for the
    /// duration of this function call.
    ///
    /// This function must be called with interrupts disabled. The implementation may re-enable them
    /// during the function call, but they will be disabled again before returning.
    ///
    /// The callee must always resume the system scheduler after calling this function.
    #[unsafe(export_name = "v5gdb_handle_debug_event")]
    #[cfg_attr(target_os = "vexos", instruction_set(arm::a32))]
    pub unsafe extern "aapcs" fn handle_debug_event(ctx: *mut DebugEventContext) {
        unsafe {
            DEBUGGER.get().unwrap().handle_debug_event(&mut *ctx);
        }
    }

    static ORIGINAL_VECTOR_ADDRESSES_SET: AtomicBool = AtomicBool::new(false);

    /// Registers a set of custom CPU exception handlers that can handle debug events.
    pub fn install_vectors() {
        unsafe extern "C" {
            #[link_name = "v5gdb_debugger_vector_table"]
            static debugger_vector_table: c_void;
            #[link_name = "v5gdb_original_vector_addresses"]
            static mut original_vector_addresses: [u32; 8];
        }

        if !ORIGINAL_VECTOR_ADDRESSES_SET.swap(true, Ordering::Relaxed) {
            let old_vbar = VectorBaseAddressRegister::read();

            critical_section::with(|_| unsafe {
                // No exceptions should be allowed to occur while updating the vector table,
                // since the vector table is responsible for handling those
                // exceptions.
                asm!("cpsid f", options(nostack, nomem, preserves_flags));

                // The default stack that VEXos gives us in abort mode is only 1kb, which is
                // extremely inadequate for what we're doing in the debug event handler, so we
                // need to load our own stack region.
                //
                // In an effort to avoid requiring linkerscript modification, we're storing this
                // stack as an uninitialized static global rather than giving it it's own
                // explicit linker section.
                asm!(
                    // abort mode
                    "cps #0b10111",
                    "ldr sp, ={abort_stack}+{stack_size}",
                    // back to sys mode
                    "cps #0b11111",
                    abort_stack = sym ABORT_STACK,
                    stack_size = const ABORT_STACK_SIZE,
                    options(nostack, preserves_flags)
                );

                original_vector_addresses =
                    array::from_fn(|i| old_vbar.ptr().byte_add(i * size_of::<u32>()) as u32);

                dsb();

                asm!("cpsie f", options(nostack, nomem, preserves_flags));
            });
        }

        unsafe {
            let overlay_table_ptr = &raw const debugger_vector_table;
            VectorBaseAddressRegister::new(overlay_table_ptr.cast()).write();
        }
    }

    impl DebugEventContext {
        /// Read the ARM instruction which the exception would return to.
        ///
        /// # Safety
        ///
        /// The caller must ensure the return address is valid for reads. This might not be the case
        /// if, for example, the exception was a prefetch abort caused by the instruction
        /// being inaccessible.
        #[must_use]
        pub unsafe fn read_instr(&self) -> Instruction {
            if self.spsr.thumb() {
                let ptr = self.program_counter as *mut u16;
                Instruction::Thumb(unsafe { ptr.read_volatile() })
            } else {
                let ptr = self.program_counter as *mut u32;
                Instruction::Arm(unsafe { ptr.read_volatile() })
            }
        }

        /// Load the address or instruction which the faulting instruction attempted to operate on.
        ///
        /// # Safety
        ///
        /// This function accesses CPU state that's set post-exception. The caller must ensure that
        /// this state has not been invalidated.
        #[must_use]
        pub unsafe fn target(&self) -> usize {
            let target: usize;

            unsafe {
                core::arch::asm!(
                    "mrc p15, 0, {ifar}, c6, c0, 1",
                    ifar = out(reg) target,
                    options(nomem, nostack, preserves_flags)
                );
            }

            target
        }
    }
}

#[cfg(target_arch = "arm")]
pub use arm::*;
