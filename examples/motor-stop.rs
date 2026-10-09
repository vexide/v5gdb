//! Motor stop on breakpoint example
//!
//! # What this demos
//!
//! This example shows the `with_motor_stop(true)` feature.
//! It:
//!
//! 1. Spin any motors that are plugged into the bot at +6 V (6000 mV)
//! 2. Runs the debugger with `with_motor_stop(true)`
//! 3. It then hits a breakpoint where all of the motors stop.
//! 4. Then resumes normally; meaning all the motors spin again.
//!
//!
//! # How to build and upload
//!
//!   ```sh
//!   cargo v5 run --example motor_stop
//!   ```
//!
//! # How to attach GDB
//!
//!   In a second terminal (The brain should be connected through usb):
//!
//!   ```sh
//!   arm-none-eabi-gdb \
//!       target/armv7a-vex-v5/debug/examples/motor_stop \
//!       -ex "target extended-remote | cargo v5 terminal"
//!   ```
//!
//!   Possibly useful gdb commands once connected:
//!
//!   ```sh
//!   (gdb) monitor help              # list all monitor commands
//!   (gdb) monitor autostop ?        # check current auto-stop setting
//!   (gdb) monitor autostop true     # enable (already on in this example but off by default)
//!   (gdb) monitor autostop false    # disable to test motors keep running
//!   (gdb) monitor stop              # manually stop motors right now
//!   (gdb) c                         # continue (motors spin again)
//!   (gdb) break motor_stop_loop     # set a named-function breakpoint
//!   (gdb) info registers            # inspect CPU registers
//!   (gdb) disconnect                # detach cleanly
//!   ```

use std::time::Duration;

use v5gdb::{
    debugger::{DebuggerConfig, V5Debugger},
    logging::LevelFilter,
    transport::StdioTransport,
};
use vex_sdk::{V5_MAX_DEVICE_PORTS, vexDeviceGetByIndex, vexDeviceMotorVoltageSet};
use vexide::prelude::*;

/// Set all detected motors to `voltage_mv` millivolts.
///
/// This mirrors the loop in `sdk::stop_all_motors`
/// Non-motor devices on a port should silently ignore this call.
/// (the SDK should no-op if the device type does not match).
fn set_all_motors(voltage_mv: i32) {
    for port in 0..V5_MAX_DEVICE_PORTS {
        unsafe {
            let dev = vexDeviceGetByIndex(port as u32);
            if dev.is_null() {
                continue;
            }

            vexDeviceMotorVoltageSet(dev, voltage_mv);
        }
    }
}

// named function so gdbs `break motor_stop_loop` command works
#[inline(never)]
fn motor_stop_loop(iteration: u32) -> u32 {
    iteration.wrapping_add(1)
}

#[vexide::main(banner(enabled = false))]
async fn main(_peripherals: Peripherals) {
    v5gdb::logging::set_max_level(LevelFilter::max());

    println!("*** v5gdb motor-stop-on-breakpoint example ***");
    println!("Motors will spin at +6 V, then stop automatically at the breakpoint.");
    println!("Connect GDB, then type 'c' to resume and watch motors restart.");

    // Installs the debugger with auto-motor stop enabled.
    //
    // `DebuggerConfig` contains the debugger's initial configuration.
    // You can also enable or disable this at runtime from GDB with:
    //
    // (gdb) monitor autostop true
    //
    // or
    //
    // (gdb) monitor autostop false

    v5gdb::install(
        V5Debugger::new(StdioTransport::new()).with_config(DebuggerConfig {
            stop_motors_on_break: true,
        }),
    );

    // Spin motors so you can watch the motors running
    println!("Spinning motors at +6 V for 2 s...");
    set_all_motors(6_000); // 6 000 mV = 6 V
    sleep(Duration::from_secs(2)).await;

    // Breakpoint
    // the motors will stop here
    println!("Triggering breakpoint -> motors should stop **now**.");
    v5gdb::breakpoint!();
    // At this point the debugger fires, `stop_all_motors()` is called inside
    // `handle_debug_event()` because `stop_motors_on_break` is equal to `true`, and then
    // the gdb console loop runs.
    // the brain waits here for new gdb commands.

    // after gdb's `c` (continue), execution resumes here.
    println!("Resumed.  Motors will spin again and loop...");

    let mut iter = 0u32;
    loop {
        set_all_motors(6_000);
        iter = motor_stop_loop(iter);
        println!("Loop iteration {iter}");
        sleep(Duration::from_secs(1)).await;
    }
}
