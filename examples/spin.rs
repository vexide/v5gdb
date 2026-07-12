//! Ctrl-C halt test.
//!
//! Usage:
//! 1. Terminal A: `v5gdb --tcp 127.0.0.1:35537` (connects to v5gdb server).
//! 2. Terminal B: `gdb target/armv7a-vex-v5/debug/examples/spin -ex "target remote :35537"`.
//! 3. In GDB: `continue`, then press Ctrl-C.
//!
//! GDB should report that the program received a SIGINT and enter the debug monitor.

use v5gdb::{debugger::V5Debugger, transport::StdioTransport};
use vex_sdk::vexTasksRun;
use vexide::prelude::*;

#[inline(never)]
fn busy_spin() -> ! {
    loop {
        for _ in 0..50_000_000u32 {
            core::hint::spin_loop();
        }
    }
}

#[vexide::main]
async fn main(_peripherals: Peripherals) {
    colored::control::set_override(true);
    clang_log::init(log::Level::max(), "v5gdb(spin)");

    v5gdb::install(V5Debugger::new(StdioTransport));

    // Stop here so GDB can attach before we get stuck.
    v5gdb::breakpoint!();

    println!("About to start spinning; attach GDB, `continue`, then press Ctrl-C");
    unsafe {
        vexTasksRun();
    }
    busy_spin();
}
