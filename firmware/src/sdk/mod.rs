//! Runtime patching of SDK functions to emulate different return values or arguments.
//!
//! This module doesn't affect the actual implementations of the underlying SDK functions, but it is
//! able to add a proxy layer between VEXos and user code via the wrapper functions defined in
//! libraries like v5rt and vex_sdk_jumptable. The wrapper functions normally get inlined into their
//! call sites when LTO is on, so the functionality in this module only works when LTO is off.

use core::{arch::global_asm, ptr};

use aarch32_cpu::asm::{dsb, isb};

use crate::cpu::cache::{self, CacheTarget};

pub mod competition;

global_asm!(include_str!("./sdk_trampoline.s"), options(raw));
unsafe extern "C" {
    /// A position-independent ARM function that jumps to another (configurable) function.
    fn v5gdb_sdk_trampoline_arm();
    /// Marks the end of the code for [`v5gdb_sdk_trampoline_arm`].
    static v5gdb_sdk_trampoline_arm_end: u32;
    /// A position-independent Thumb function that jumps to another (configurable) function.
    fn v5gdb_sdk_trampoline_thumb();
    /// Marks the end of the code for [`v5gdb_sdk_trampoline_thumb`].
    static v5gdb_sdk_trampoline_thumb_end: u32;
}

/// Overwrite the target function to branch to the given proxy when called instead of performing
/// its original functionality.
///
/// The target function's instruction set is detected via its Thumb bit and a matching trampoline is
/// installed.
///
/// # Safety
///
/// The target function must be at least 3 words long, properly aligned for its instruction set, and
/// valid to write to. The destination function must be valid to call in all the same situations as
/// the target function and also have the same signature as it.
pub unsafe fn redirect_function(target: *mut (), destination: *const ()) {
    const THUMB_BIT: usize = 0b1;
    let is_thumb = (target as usize) & THUMB_BIT != 0;

    let (trampoline_fn, trampoline_end) = if is_thumb {
        (
            v5gdb_sdk_trampoline_thumb as unsafe extern "C" fn(),
            &raw const v5gdb_sdk_trampoline_thumb_end,
        )
    } else {
        (
            v5gdb_sdk_trampoline_arm as unsafe extern "C" fn(),
            &raw const v5gdb_sdk_trampoline_arm_end,
        )
    };

    // We cast to u16 since the target function may be a 2-byte aligned Thumb function.
    let trampoline_src = ((trampoline_fn as usize) & !THUMB_BIT) as *const u16;
    let write_addr = ((target as usize) & !THUMB_BIT) as *mut u16;

    let code_len_bytes = (trampoline_end as usize) - (trampoline_src as usize);
    let code_len = code_len_bytes / size_of::<u16>();
    let destination_slot = unsafe { write_addr.add(code_len) };

    unsafe {
        ptr::copy_nonoverlapping(trampoline_src, write_addr, code_len);
        // Keep the destination's Thumb bit intact so the trampoline's `bx` enters it in the
        // correct instruction set.
        ptr::write_unaligned(destination_slot.cast::<u32>(), destination as u32);
    }

    dsb();
    isb();

    // Sync both start and end, in case the function crosses a cache line.
    cache::sync_instruction(CacheTarget::Address(write_addr as u32));
    cache::sync_instruction(CacheTarget::Address(destination_slot as u32));
}

/// Directly access VEX SDK functions over the jumptable without their wrappers.
///
/// This is effectively a partial re-implementation of the `vex-sdk-jumptable` crate, which we can't
/// use here because those might be the functions we are redirecting. If we were to call those
/// directly, it might cause an infinite loop.
macro_rules! jumptable {
    ($offset:literal, $ty:ty) => {{
        const JUMPTABLE_BASE: u32 = 0x037fc000;
        let ptr = (JUMPTABLE_BASE + $offset) as *const $ty;
        *ptr
    }};
}
pub(crate) use jumptable;

/// Immediately stops every motor connected to the brain by setting its voltage to 0 mV.
///
/// Ports that have no device connected are silently skipped. The loop does **not** short circuit
/// on a null device handle, so noncontiguous motor configurations (e.g. motors on ports 1 and 5
/// with nothing on 2-4) are handled correctly.
pub fn stop_all_motors() {
    use vex_sdk::{V5_MAX_DEVICE_PORTS, vexDeviceGetByIndex, vexDeviceMotorVoltageSet};
    for port_num in 0..V5_MAX_DEVICE_PORTS {
        unsafe {
            let device = vexDeviceGetByIndex(port_num as u32);
            if device.is_null() {
                // Nothing plugged in to this port skip but keep iterating.
                // Must not `break` here: ports can be non-contiguous, so a null handle does not
                // mean there are no more devices
                continue;
            }
            // setting voltage to 0mV immediately cuts power to the motor
            // this works for both 11W and 5.5W smart-motors
            vexDeviceMotorVoltageSet(device, 0);
        }
    }
}
/// System serial I/O.
///
/// See the `vex-sdk-jumptable` crate for docs on jumptable functions.
pub mod serial {
    use core::cmp;

    use derive_more::From;

    /// The size of the serial output ringbuffer.
    pub const OUT_BUF_SIZE: usize = 2048;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, From)]
    #[repr(transparent)]
    pub struct Channel(pub u32);

    impl Channel {
        /// The VEXos user serial channel (supports read + write).
        ///
        /// This is the only channel that can be easily accessed by end users. The V5 Brain
        /// publishes it via USB CDC ACM on its Micro-USB port.
        pub const USER: Self = Self(1);
    }

    /// Writes a byte to the given channel.
    ///
    /// Returns whether the byte was written.
    pub fn write_byte(channel: Channel, byte: u8) -> bool {
        let sys_write_char = unsafe { jumptable!(0x898, extern "C" fn(u32, u8) -> i32) };
        sys_write_char(channel.0, byte) != 0
    }

    /// Writes some bytes from a buffer to the given channel.
    ///
    /// Returns how many bytes were written.
    ///
    /// # Safety
    ///
    /// The buffer must be valid for reads and be of the specified length.
    ///
    /// # Errors
    ///
    /// Returns an error if the channel is invalid.
    pub unsafe fn write_buf(channel: Channel, buf: *const u8, len: usize) -> Result<usize, ()> {
        let sys_write_buf =
            unsafe { jumptable!(0x89c, unsafe extern "C" fn(u32, *const u8, u32) -> i32) };

        let written = unsafe { sys_write_buf(channel.0, buf, len as u32) };
        if written == -1 {
            Err(())
        } else {
            Ok(written as usize)
        }
    }

    /// Writes some bytes from the given slice to the specified channel.
    ///
    /// Returns how many bytes were written.
    ///
    /// # Errors
    ///
    /// Returns an error if the channel is invalid.
    pub fn write(channel: Channel, slice: &[u8]) -> Result<usize, ()> {
        unsafe { write_buf(channel, slice.as_ptr(), slice.len()) }
    }

    /// Writes the entire buffer to the specified channel.
    ///
    /// This function will block until all bytes were written.
    ///
    /// # Errors
    ///
    /// Returns an error if the channel is invalid.
    pub fn write_all(channel: Channel, mut slice: &[u8]) -> Result<(), ()> {
        loop {
            let written = write(channel, slice)?;
            slice = &slice[cmp::min(written, slice.len())..];

            if slice.is_empty() {
                break;
            }

            unsafe {
                vex_sdk::vexTasksRun();
            }
        }

        Ok(())
    }

    /// Reads a single byte from the specified channel.
    ///
    /// Returns `None` if there was no byte to read.
    pub fn read_byte(channel: Channel) -> Option<u8> {
        let sys_read = unsafe { jumptable!(0x8a0, extern "C" fn(u32) -> i32) };
        match sys_read(channel.0) {
            -1 => None,
            byte => Some(byte as u8),
        }
    }

    /// Reads a single byte from the specified channel without popping it from the read queue.
    ///
    /// Returns `None` if there was no byte to read.
    pub fn peek_byte(channel: Channel) -> Option<u8> {
        let sys_peek = unsafe { jumptable!(0x8a4, extern "C" fn(u32) -> i32) };
        match sys_peek(channel.0) {
            -1 => None,
            byte => Some(byte as u8),
        }
    }

    /// Gets the number of unused bytes in the serial output ringbuffer.
    pub fn write_buf_capacity(channel: Channel) -> Option<usize> {
        let sys_write_free = unsafe { jumptable!(0x8ac, extern "C" fn(u32) -> i32) };
        match sys_write_free(channel.0) {
            -1 => None,
            len => Some(len as usize),
        }
    }
}
