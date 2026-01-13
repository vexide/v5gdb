use std::{any::Any, convert::Infallible, hint::black_box, time::Duration};

use v5gdb::{
    DEBUGGER,
    debugger::V5Debugger,
    gdb_target::arch::{
        ArmBreakpointKind,
        hw::{HwBreakpointManager, Specificity},
    },
    transport::StdioTransport,
};
use vex_sdk::{vexSerialReadChar, vexTasksRun};
use vexide::prelude::*;

#[inline(never)]
fn fib(n: u64) -> u64 {
    let mut a = 1;
    let mut b = 0;
    let mut count = 0;

    while count < n {
        let tmp = a + b;
        b = a;
        a = tmp;
        count += 1;
    }

    b
}

#[vexide::main(banner(enabled = false))]
async fn main(_peripherals: Peripherals) {
    v5gdb::install(V5Debugger::new(StdioTransport::new()));

    println!("Hello, world");
    v5gdb::breakpoint();

    let n = fib(black_box(40));
    println!("{n}");
}
