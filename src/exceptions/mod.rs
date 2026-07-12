use crate::cpu::ProgramStatus;

/// All the data captured during an exception about the previous state of the program.
///
/// These fields are placed in a specific order so that `overlay.S` builds the struct properly.
#[derive(Debug, Clone, Default, PartialEq)]
#[repr(C)]
pub struct DebugEventContext {
    /// The Program Status Register from before the exception.
    pub cpsr: ProgramStatus,
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

impl Registers for DebugEventContext {
    type ProgramCounter = u32;

    fn pc(&self) -> Self::ProgramCounter {
        self.program_counter
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        let mut send = move |bytes: &[u8]| {
            for &b in bytes {
                write_byte(Some(b));
            }
        };

        for r in self.registers {
            send(&r.to_le_bytes());
        }

        send(&self.stack_pointer.to_le_bytes());
        send(&self.link_register.to_le_bytes());
        send(&self.program_counter.to_le_bytes());
        send(&self.cpsr.raw_value().to_le_bytes());

        for d in self.vfp_registers {
            send(&d.to_le_bytes());
        }

        send(&self.fpscr.to_le_bytes());
    }

    fn gdb_deserialize(&mut self, mut bytes: &[u8]) -> Result<(), ()> {
        fn read<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], ()> {
            let Some((left, right)) = bytes.split_at_checked(N) else {
                return Err(());
            };
            *bytes = right;

            Ok(<[u8; N]>::try_from(left).unwrap())
        }

        for r in &mut self.registers {
            *r = u32::from_le_bytes(read(&mut bytes)?);
        }

        self.stack_pointer = u32::from_le_bytes(read(&mut bytes)?);
        self.link_register = u32::from_le_bytes(read(&mut bytes)?);
        self.program_counter = u32::from_le_bytes(read(&mut bytes)?);
        self.cpsr = ProgramStatus::new_with_raw_value(u32::from_le_bytes(read(&mut bytes)?));

        for d in &mut self.vfp_registers {
            *d = u64::from_le_bytes(read(&mut bytes)?);
        }

        self.fpscr = u32::from_le_bytes(read(&mut bytes)?);

        Ok(())
    }
}

#[cfg(target_arch = "arm")]
pub(crate) mod arm {
    use core::{
        arch::asm,
        array,
        ffi::c_void,
        mem::MaybeUninit,
        sync::atomic::{AtomicBool, AtomicU32, Ordering},
    };

    use aarch32_cpu::asm::dsb;

    use crate::{
        DEBUGGER,
        cpu::{exception::VectorBaseAddressRegister, instruction::Instruction},
        exceptions::DebugEventContext,
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
    pub unsafe extern "aapcs" fn handle_debug_event(ctx: *mut DebugEventContext) -> bool {
        unsafe { DEBUGGER.get().unwrap().handle_debug_event(&mut *ctx) }
    }

    /// Minimum interval, in milliseconds, between calls to `Debugger::poll`.
    const IRQ_POLL_INTERVAL_MS: u32 = 10;

    /// The system time (ms) at which the IRQ hook last ran `Debugger::poll`.
    static LAST_POLL_TIME_MS: AtomicU32 = AtomicU32::new(0);

    /// Total number of times [`irq_poll`] has been invoked (# of IRQs intercepted).
    static IRQ_POLL_COUNT: AtomicU32 = AtomicU32::new(0);

    /// Gets diagnostics regarding the IRQ hook. This can be used to ensure the IRQ handler is
    /// actually being invoked.
    #[must_use]
    pub fn irq_poll_stats() -> IrqPollStats {
        IrqPollStats {
            poll_count: IRQ_POLL_COUNT.load(Ordering::Relaxed),
            last_poll_ms: LAST_POLL_TIME_MS.load(Ordering::Relaxed),
        }
    }

    /// Snapshot of the IRQ hook diagnostics returned by [`irq_poll_stats`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IrqPollStats {
        /// Total number of IRQs intercepted.
        pub poll_count: u32,
        /// System time (ms) of the last `Debugger::poll`.
        pub last_poll_ms: u32,
    }

    /// IRQ handler callback.
    ///
    /// This is called from `v5gdb_irq_handler` at the beginning of every IRQ exception and is
    /// responsible for periodically invoking [`Debugger::poll`](crate::Debugger::poll).
    ///
    /// # Notes
    ///
    /// This runs in interrupt context on VEXos's 8 KiB IRQ-mode stack, so it must remain fairly
    /// lightweight with no allocation or blocking or calls to non-thread-safe functions.
    #[unsafe(export_name = "v5gdb_irq_poll")]
    #[cfg_attr(target_os = "vexos", instruction_set(arm::a32))]
    pub extern "aapcs" fn irq_poll() {
        IRQ_POLL_COUNT.fetch_add(1, Ordering::Relaxed);

        let now = unsafe { vex_sdk::vexSystemTimeGet() };
        let last = LAST_POLL_TIME_MS.load(Ordering::Relaxed);

        // `wrapping_sub` keeps this correct across a u32 millisecond wraparound.
        if now.wrapping_sub(last) < IRQ_POLL_INTERVAL_MS {
            return;
        }
        LAST_POLL_TIME_MS.store(now, Ordering::Relaxed);

        if let Some(debugger) = DEBUGGER.get() {
            debugger.poll();
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
        /// The caller must ensure the return address is valid for volatile reads. This might not be
        /// the case if, for example, the exception was a prefetch abort caused by the instruction
        /// being inaccessible.
        #[must_use]
        pub unsafe fn read_instr(&self) -> Instruction {
            let ptr = self.program_counter as *mut u32;
            unsafe { Instruction::read(ptr, self.cpsr.thumb()) }
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
use gdbstub::arch::Registers;
