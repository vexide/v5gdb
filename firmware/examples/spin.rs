//! Ctrl-C halt test.
//!
//! Usage:
//! 1. Start the program.
//! 2. Connect with `v5gdb target/armv7a-vex-v5/debug/examples/spin`
//! 3. In GDB: `continue`, then press Ctrl-C.
//!
//! GDB should report that the program received a SIGINT and enter the debug monitor.

use v5gdb::prelude::*;
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
    let log_level = if option_env!("DEBUG").is_some() {
        LevelFilter::max()
    } else {
        LevelFilter::Warn
    };
    v5gdb::install(V5Debugger::stdio().log_level(log_level));

    unsafe {
        vexTasksRun();
    }
    busy_spin();
}
