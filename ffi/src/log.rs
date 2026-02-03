use core::{fmt, fmt::Write};

use log::{Log, Metadata, Record};
use owo_colors::{AnsiColors, OwoColorize};
use v5gdb::transport::mux::OUT_BUFFER_SIZE;
use vex_sdk::vexSerialWriteBuffer;

struct SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let level = record.level();
        let level = match level {
            log::Level::Error => level.color(AnsiColors::Red),
            log::Level::Warn => level.color(AnsiColors::Yellow),
            log::Level::Info => level.color(AnsiColors::Blue),
            log::Level::Debug => level.color(AnsiColors::Magenta),
            log::Level::Trace => level.color(AnsiColors::BrightBlack),
        };
        _ = writeln!(VexSerial, "v5gdb: {}: {}", level.bold(), record.args());
    }

    fn flush(&self) {
        unsafe {
            while vex_sdk::vexSerialWriteFree(1) < OUT_BUFFER_SIZE as i32 {
                vex_sdk::vexTasksRun();
            }
        }
    }
}

static LOGGER: SimpleLogger = SimpleLogger;

pub fn init() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Warn);
}

struct VexSerial;
impl fmt::Write for VexSerial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        unsafe {
            vexSerialWriteBuffer(1, s.as_ptr(), s.len() as u32);
        }
        Ok(())
    }
}
