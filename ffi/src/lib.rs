//! C FFI interface for v5gdb.

#![no_std]

use core::{
    arch::global_asm,
    ffi::{CStr, c_char, c_void},
};

use gdbstub::conn::{Connection, ConnectionExt};
use spin::Once;
use v5gdb::{
    debugger::V5Debugger,
    logging::LevelFilter,
    transport::{StdioTransport, Transport, TransportError},
};

mod panic;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub enum ReadResult {
    /// The specified byte has been read.
    Ok(u8),
    /// The transport stream encountered an error.
    Err(*const c_char),
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub enum PeekResult {
    /// The specified byte has been read.
    Ok(u8),
    /// There are no more bytes ready to read.
    Empty,
    /// The transport stream encountered an error.
    Err(*const c_char),
}

/// A custom transport method for communicating with GDB.
///
/// The contained type must be valid to transfer across a thread boundary. For example, accesses to
/// values used outside of the debugger must be atomic.
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
    pub peek_byte: unsafe extern "C" fn(data: *mut c_void) -> PeekResult,
    /// Reads the next byte received from GDB.
    pub read_byte: unsafe extern "C" fn(data: *mut c_void) -> ReadResult,
}

unsafe impl Send for TransportImpl {}

// SAFETY: the FFI consumer is responsible for ensuring peek_byte/read_byte are
// safe to call from an interrupt context.
unsafe impl Transport for TransportImpl {}

impl Connection for TransportImpl {
    type Error = TransportError;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        self.write_all(&[byte])
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        unsafe {
            let error = (self.write_buf)(self.data, buf.as_ptr(), buf.len());
            if error.is_null() {
                Ok(())
            } else {
                Err(wrap_err(error))
            }
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        unsafe {
            let error = (self.flush)(self.data);
            if error.is_null() {
                Ok(())
            } else {
                Err(wrap_err(error))
            }
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
        let result = unsafe { (self.peek_byte)(self.data) };

        match result {
            PeekResult::Ok(byte) => Ok(Some(byte)),
            PeekResult::Empty => Ok(None),
            PeekResult::Err(error) => Err(unsafe { wrap_err(error) }),
        }
    }

    fn read(&mut self) -> Result<u8, Self::Error> {
        let result = unsafe { (self.read_byte)(self.data) };

        match result {
            ReadResult::Ok(byte) => Ok(byte),
            ReadResult::Err(error) => Err(unsafe { wrap_err(error) }),
        }
    }
}

unsafe fn wrap_err(maybe_error: *const c_char) -> TransportError {
    let error = unsafe { CStr::from_ptr(maybe_error) };
    TransportError(error.to_str().unwrap_or("<error with invalid utf8>"))
}

/// Install the debugger, communicating with GDB over the V5's USB serial port.
#[unsafe(export_name = "v5gdb_install_stdio")]
pub extern "C" fn install_stdio() {
    static DEBUGGER: Once<V5Debugger<StdioTransport>> = Once::new();
    DEBUGGER.call_once(|| V5Debugger::new(StdioTransport::new()));
    v5gdb::install_by_ref(DEBUGGER.get().unwrap());
}

/// Install the debugger with a custom transport method for communicating with GDB.
///
/// # Safety
///
/// The transport's [`peek_byte`](TransportImpl::peek_byte) and
/// [`read_byte`](TransportImpl::read_byte) implementations must be safe to call from an interrupt
/// context.
#[unsafe(export_name = "v5gdb_install_custom")]
pub unsafe extern "C" fn install_custom(transport: TransportImpl) {
    static DEBUGGER: Once<V5Debugger<TransportImpl>> = Once::new();
    DEBUGGER.call_once(|| V5Debugger::new(transport));
    v5gdb::install_by_ref(DEBUGGER.get().unwrap());
}

/// Manually triggers a breakpoint.
#[unsafe(export_name = "v5gdb_breakpoint")]
pub extern "C" fn breakpoint() {
    v5gdb::breakpoint!();
}

#[repr(C)]
pub enum LogFilter {
    /// Disables all log messages.
    Off,
    /// Disables log messages less severe than `Error`.
    Error,
    /// Disables log messages less severe than `Warn`.
    Warn,
    /// Disables log messages less severe than `Info`.
    Info,
    /// Disables log messages less severe than `Debug`.
    Debug,
    /// Enables all log messages.
    Trace,
}

impl From<LogFilter> for LevelFilter {
    fn from(value: LogFilter) -> Self {
        match value {
            LogFilter::Off => LevelFilter::Off,
            LogFilter::Error => LevelFilter::Error,
            LogFilter::Warn => LevelFilter::Warn,
            LogFilter::Info => LevelFilter::Info,
            LogFilter::Debug => LevelFilter::Debug,
            LogFilter::Trace => LevelFilter::Trace,
        }
    }
}

/// Sets the most verbose level of debugger log messages that will be printed.
///
/// Defaults to [`LevelFilter::Warn`]. This can also be changed at runtime with the `monitor log`
/// command.
#[unsafe(export_name = "v5gdb_set_max_log_level")]
pub extern "C" fn set_max_log_level(level: LogFilter) {
    v5gdb::logging::set_max_level(level.into());
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
