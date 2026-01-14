use std::hint::black_box;

use v5gdb::{debugger::V5Debugger, transport::StdioTransport};
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
