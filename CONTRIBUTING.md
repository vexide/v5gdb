# Guidelines for Contributors

Thanks for taking the time to contribute to v5gdb. If you have any questions about this project or would like to get in touch with the maintainers, you should join [vexide's Discord server](https://discord.gg/d4uazRf2Nh).

## Development Environment

v5dbg follows conventions for Rust projects: it's built with Cargo, formatted with Rustfmt, and linted with Clippy.

Since Rust's support for the VEX V5 platform is unstable, we only support some versions of Rust. When you first clone the project, you should configure your system to use the correct version by running this command:

```
rustup toolchain install
```

The repository has two Cargo workspaces:

- `firmware/` contains everything that runs on-device, including the debugger itself and FFI bindings.
- Packages in the repository root contain code that runs on your computer (e.g. `xtask`, `cli`).

After installing the toolchain, you can build the debugger library by running `cargo build` in the `firmware` directory.

### Examples

The `firmware/examples` folder contains some example code (based on the vexide framework) that you can try debugging. To upload one of the examples to a V5 robot, first install [cargo-v5](https://github.com/vexide/cargo-v5#readme), then connect the robot to your computer with a USB cable, and finally run this command from the `firmware` directory:

```bash
cargo v5 upload --example basic
```

### Connecting with GDB

Use the `v5gdb` wrapper to connect to GDB (the code for it is in `cli/`). You can install it with:

```bash
cargo install --path cli
```

Then use it like this to connect to a robot stopped at a breakpoint:

```
$ v5gdb ./firmware/target/armv7a-vex-v5/debug/examples/basic
GNU gdb (GDB) 16.3
(gdb)
```

Alternatively, you can run the command-line tool as a server that a normal GDB instance can connect to:

```
$ v5gdb --tcp 127.0.0.1:35537
(In a different terminal window:)
$ gdb -ex "target remote :35537" bin/monolith.elf
```

Once you've established a connection, here are some GDB commands you might want to try out:

- `layout asm` or `layout src` will show the current code the processor is running.
    - After running the above command, `focus cmd` will let you use the up and down arrow keys to quickly select a previous command.
    - `tui disable` will undo the layout command.
- `set debug remote 1` will make GDB print out all its communications with the debugger.
- `break funcname` will set a software breakpoint on the `funcname` function.
- `si` will step execution forward by an instruction.
- `monitor help` will show the special commands specific to the vexide debugger.
- `cont` will continue execution as normal.
- `disconnect` will disconnect unceremoniously.

If you want to call functions or access values in your program from GDB, you can do so like this:

```
call basic::fib(40)
print $r0
(vexide:)
call vex_sdk_jumptable::display::vexDisplayForegroundColor(0xFF00FF)
(PROS:)
call (void)vexDisplayForegroundColor(0xFF00FF)
```

## Troubleshooting

If you are getting weird errors when you try connecting with GDB (or your robot crashes when you do so), you should make sure your GDB version supports the `armv7` architecture by running the command `set architecture`.

If you suspect there is an issue with the serial connection and you're not sure what GDB is really doing, run the command `set debug remote 1` which will start printing out all the communications between GDB and v5gdb in real-time.

If you want an extremely verbose log of everything v5gdb is sending to GDB, you can connect like this:

```
v5gdb --debug-io bin/monolith.elf
```

## Integration tests

You can run integration tests by connecting to a brain and running `cargo xtask test` from the repository root. It will upload each test in the `firmware/tests` directory and run them. The first time you run it, it will take a while to compile the code in the `xtask` directory.

## Licensing

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed under the MIT and Apache-2.0 licenses, without any additional terms or conditions.
