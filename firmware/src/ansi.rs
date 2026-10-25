//! ANSI escape codes for styling terminal output.

#![allow(dead_code, reason = "some colors are only used with certain features")]

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const BLUE: &str = "\x1b[34m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BRIGHT_BLACK: &str = "\x1b[90m";
