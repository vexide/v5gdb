//! Reads serial input and prints each line back, to test sending input to a program while the
//! debugger is installed.
//!
//! Run `v5gdb --terminal` (or `v5gdb --tcp ...`) and type into it.

use std::{
    io::{Read, stdin},
    time::Duration,
};

use v5gdb::prelude::*;
use vexide::prelude::*;

#[vexide::main]
async fn main(_peripherals: Peripherals) {
    v5gdb::install(V5Debugger::stdio());

    println!("Type something:");

    let mut line = Vec::new();
    let mut buf = [0; 64];
    loop {
        // vexide's stdin returns 0 bytes when nothing is available, instead of blocking.
        let size = stdin().read(&mut buf).unwrap();
        for &byte in &buf[..size] {
            if byte == b'\n' {
                println!("echo: {}", String::from_utf8_lossy(&line));
                line.clear();
            } else {
                line.push(byte);
            }
        }

        sleep(Duration::from_millis(10)).await;
    }
}
