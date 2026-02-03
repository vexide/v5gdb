use std::time::Duration;

use v5gdb::{DEBUGGER, debugger::V5Debugger, transport::StdioTransport};
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

#[vexide::main]
async fn main(_peripherals: Peripherals) {
    colored::control::set_override(true);
    clang_log::init(log::Level::max(), "v5gdb(basic)");

    v5gdb::install(V5Debugger::new(StdioTransport));
    v5gdb::breakpoint!();

    loop {
        let num = 40;
        let x = fib(num);
        println!("{x}");
        sleep(Duration::from_secs(1)).await;
    }
}
