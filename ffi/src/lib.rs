//! C FFI interface for v5gdb.

#![no_std]

use core::{
    arch::global_asm,
    ffi::{CStr, c_char, c_void},
    ptr,
};

use gdbstub::conn::{Connection, ConnectionExt};
use spin::Once;
use v5gdb::{
    debugger::V5Debugger,
    transport::{StdioTransport, TransportError},
};

mod log;
mod panic;

/// A custom transport method for communicating with GDB.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TransportImpl {
    /// Custom data, passed to each function.
    pub data: *mut c_void,
    /// One-time initialize callback. Called on first breakpoint.
    pub initialize: unsafe extern "C" fn(data: *mut c_void),
    /// Write a buffer containing packet data to GDB.
    ///
    /// Returns a static error string if an error occurred, or null if the operation was
    /// successful.
    pub write_buf:
        unsafe extern "C" fn(data: *mut c_void, buf: *const u8, len: usize) -> *const c_char,
    /// Flushes any pending writes to GDB.
    ///
    /// Returns a static error string if an error occurred, or null if the operation was
    /// successful.
    pub flush: unsafe extern "C" fn(data: *mut c_void) -> *const c_char,
    /// Peeks the next byte received from GDB.
    ///
    /// Returns -1 if there are no bytes to read, or returns the unsigned byte.
    /// Sets *error to a static error string if an error occurred.
    pub peek_byte: unsafe extern "C" fn(data: *mut c_void, error: *mut *const c_char) -> i32,
    /// Reads the next byte received from GDB.
    ///
    /// Sets *error to a static error string if an error occurred.
    pub read_byte: unsafe extern "C" fn(data: *mut c_void, error: *mut *const c_char) -> u8,
}

impl TransportImpl {
    unsafe fn wrap_err(maybe_error: *const c_char) -> Result<(), TransportError> {
        if maybe_error.is_null() {
            Ok(())
        } else {
            let error = unsafe { CStr::from_ptr(maybe_error) };
            Err(TransportError(
                error.to_str().unwrap_or("<error with invalid utf8>"),
            ))
        }
    }
}

unsafe impl Send for TransportImpl {}
unsafe impl Sync for TransportImpl {}

impl Connection for TransportImpl {
    type Error = TransportError;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        self.write_all(&[byte])
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        unsafe {
            let error = (self.write_buf)(self.data, buf.as_ptr(), buf.len());
            Self::wrap_err(error)
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        unsafe {
            let error = (self.flush)(self.data);
            Self::wrap_err(error)
        }
    }

    fn on_session_start(&mut self) -> Result<(), Self::Error> {
        unsafe {
            (self.initialize)(self.data);
        }
        Ok(())
    }
}

impl ConnectionExt for TransportImpl {
    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        let mut error = ptr::null();
        let byte = unsafe { (self.peek_byte)(self.data, &raw mut error) };

        unsafe {
            Self::wrap_err(error)?;
        }

        if byte > 0 {
            Ok(Some(byte as u8))
        } else {
            Ok(None)
        }
    }

    fn read(&mut self) -> Result<u8, Self::Error> {
        let mut error = ptr::null();
        let byte = unsafe { (self.read_byte)(self.data, &raw mut error) };

        unsafe {
            Self::wrap_err(error)?;
        }
        Ok(byte)
    }
}

/// Install the debugger, communicating with GDB over the V5's USB serial port.
#[unsafe(export_name = "v5gdb_install_stdio")]
pub extern "C" fn install_stdio() {
    self::log::init();
    static DEBUGGER: Once<V5Debugger<StdioTransport>> = Once::new();
    DEBUGGER.call_once(|| V5Debugger::new(StdioTransport));
    v5gdb::install_by_ref(DEBUGGER.get().unwrap());
}

/// Install the debugger with a custom transport method for communicating with GDB.
#[unsafe(export_name = "v5gdb_install_custom")]
pub extern "C" fn install_custom(transport: TransportImpl) {
    self::log::init();
    static DEBUGGER: Once<V5Debugger<TransportImpl>> = Once::new();
    DEBUGGER.call_once(|| V5Debugger::new(transport));
    v5gdb::install_by_ref(DEBUGGER.get().unwrap());
}

/// Manually triggers a breakpoint.
#[unsafe(export_name = "v5gdb_breakpoint")]
pub extern "C" fn breakpoint() {
    v5gdb::breakpoint!();
}

// In the VEX partner SDK, vexTasksRun is renamed to vexBackgroundProcessing.
// We add a weak alias for vexBackgroundProcessing as vexTasksRun in case we're in that environment.
global_asm!(
    "
.text
.arm
.weak vexTasksRun
vexTasksRun:
    b vexBackgroundProcessing
"
);
