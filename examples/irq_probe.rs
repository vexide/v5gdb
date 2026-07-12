//! Probe to verify the overlay IRQ hook is actually being invoked.

use std::time::Duration;

use v5gdb::{
    debugger::{V5Debugger, async_halt_request_count},
    exceptions::irq_poll_stats,
    transport::StdioTransport,
};
use vexide::prelude::*;

#[vexide::main]
async fn main(_peripherals: Peripherals) {
    colored::control::set_override(true);
    clang_log::init(log::Level::max(), "v5gdb(irq_probe)");

    v5gdb::install(V5Debugger::new(StdioTransport));

    loop {
        let stats = irq_poll_stats();
        println!(
            "irq_poll: count={}, last_poll_ms={}, ctrl_c={}",
            stats.poll_count,
            stats.last_poll_ms,
            async_halt_request_count(),
        );
        sleep(Duration::from_secs(2)).await;
    }
}
