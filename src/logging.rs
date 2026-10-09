//! Debugger logging facilities.
//!
//! The default log level is [`Level::Warn`], but this is configurable via [`set_max_level`].
//!
//! v5gdb doesn't use the global loggers from `log` or `tracing` because user loggers usually
//! aren't reentrant. For example, a logger that calls `std::println!` will panic if a breakpoint is
//! hit while something is already printing, since the debugger would then try to log from inside
//! that `println!` call. Instead, log messages are formatted into a buffer on the stack and written
//! directly to serial, which doesn't require any locks.

use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicUsize, Ordering},
};

pub use log::{Level, LevelFilter};
use owo_colors::{AnsiColors, OwoColorize};

static MAX_LEVEL: AtomicUsize = AtomicUsize::new(LevelFilter::Warn as usize);

/// Sets the most verbose level of debugger log messages that will be printed.
///
/// Defaults to [`LevelFilter::Warn`]. This can also be changed at runtime with the `monitor log`
/// command.
pub fn set_max_level(level: LevelFilter) {
    MAX_LEVEL.store(level as usize, Ordering::Relaxed);
}

/// Returns the most verbose level of debugger log messages that will be printed.
#[must_use]
pub fn max_level() -> LevelFilter {
    match MAX_LEVEL.load(Ordering::Relaxed) {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

/// Prints a debugger log message to serial if `level` is enabled.
///
/// This should not be used from user code.
#[doc(hidden)]
pub fn log(level: Level, args: fmt::Arguments<'_>) {
    if level > max_level() {
        return;
    }

    let colored_level = match level {
        Level::Error => level.color(AnsiColors::Red),
        Level::Warn => level.color(AnsiColors::Yellow),
        Level::Info => level.color(AnsiColors::Blue),
        Level::Debug => level.color(AnsiColors::Magenta),
        Level::Trace => level.color(AnsiColors::BrightBlack),
    };

    let mut writer = BufferedSerial::new();
    _ = writeln!(writer, "v5gdb: {}: {args}", colored_level.bold());
    writer.flush();
}

macro_rules! event {
    ($level:ident, $($arg:tt)+) => {
        $crate::logging::log($crate::logging::Level::$level, format_args!($($arg)+))
    };
}

macro_rules! error {
    ($($arg:tt)+) => { $crate::logging::event!(Error, $($arg)+) };
}

// This has an underscore to not conflict with #![warn(...)].
macro_rules! warn_ {
    ($($arg:tt)+) => { $crate::logging::event!(Warn, $($arg)+) };
}

macro_rules! info {
    ($($arg:tt)+) => { $crate::logging::event!(Info, $($arg)+) };
}

macro_rules! debug {
    ($($arg:tt)+) => { $crate::logging::event!(Debug, $($arg)+) };
}

#[allow(unused_imports)]
pub(crate) use {debug, error, event, info, warn_ as warn};

/// Collects formatted output on the stack so that log writes are grouped together into lines.
struct BufferedSerial {
    buf: [u8; Self::SIZE],
    len: usize,
}

impl BufferedSerial {
    /// The maximum number of bytes that will be buffered before flushing.
    ///
    /// This is fairly small so logs can be written from tasks with very little stack.
    const SIZE: usize = 96;

    const fn new() -> Self {
        Self {
            buf: [0; Self::SIZE],
            len: 0,
        }
    }

    fn flush(&mut self) {
        write_serial(&self.buf[..self.len]);
        self.len = 0;
    }
}

impl Write for BufferedSerial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut src = s.as_bytes();
        while !src.is_empty() {
            if self.len == Self::SIZE {
                self.flush();
            }

            let nbytes = src.len().min(Self::SIZE - self.len);
            self.buf[self.len..self.len + nbytes].copy_from_slice(&src[..nbytes]);
            self.len += nbytes;
            src = &src[nbytes..];
        }
        Ok(())
    }
}

impl Drop for BufferedSerial {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(target_arch = "arm")]
fn write_serial(bytes: &[u8]) {
    use crate::{
        sdk::serial::{Channel, write_buf_capacity},
        transport::mux::flush_serial,
    };

    if bytes.is_empty() {
        return;
    }
    if write_buf_capacity(Channel::USER).unwrap() < bytes.len() {
        flush_serial();
    }

    unsafe {
        vex_sdk::vexSerialWriteBuffer(Channel::USER.0, bytes.as_ptr(), bytes.len() as u32);
    }
    flush_serial();
}

#[cfg(not(target_arch = "arm"))]
fn write_serial(_bytes: &[u8]) {}
