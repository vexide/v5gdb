# Guidelines for Contributors

Thanks for taking the time to contribute to v5gdb. If you have any questions about this project or would like to get in touch with the maintainers, you should join [vexide's Discord server](https://discord.gg/d4uazRf2Nh).

## Development Environment

v5dbg follows conventions for Rust projects: it's built with Cargo, formatted with Rustfmt, and linted with Clippy.

Since Rust's support for the VEX V5 platform is unstable, we only support some versions of Rust. When you first clone the project, you should configure your system to use the correct version by running this command:

```
rustup toolchain install
```

After doing so, you can build the library with `cargo build`.

### Examples

The `examples` folder contains some example code (based on the vexide framework) that you can try debugging. To upload one of the examples to a V5 robot, first install [cargo-v5](https://github.com/vexide/cargo-v5#readme), then connect the robot to your computer with a USB cable, and finally run this command:

```bash
cargo v5 upload --example basic
```

> [!WARNING]
>
> At the time of writing, the latest release of cargo-v5 has some issues with buffering serial data it receives over USB. For GDB sessions to work properly, you should install it from the `fix/stdin-buffering` branch.
>
> Here is the command you should run to install cargo-v5:
>
> ```bash
> cargo install --git https://github.com/vexide/cargo-v5 --branch fix/stdin-buffering
> ```

### Connecting with GDB

When an example program halts due to a breakpoint, start a GDB session on your computer and use cargo-v5 to stream serial output from the robot over its USB cable.

You should use the `file` command to tell GDB which example program you're debugging.

```
$ gdb
GNU gdb (GDB) 16.3
(gdb) file ./target/armv7a-vex-v5/debug/examples/basic
Reading symbols from ./target/armv7a-vex-v5/debug/examples/basic...
(gdb) target remote | cargo v5 terminal
Remote debugging using | cargo v5 terminal
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

## Troubleshooting

If you are getting weird errors when you try connecting with GDB (or your robot crashes when you do so), you should make sure your GDB version supports the `armv7` architecture by running the command `set architecture`.

If you suspect there is an issue with the serial connection and you're not sure what GDB is really doing, run the command `set debug remote 1` which will start printing out all the communications between GDB and v5gdb in real-time.

If you want a log of everything v5gdb is sending to GDB (e.g. to see panic messages or aborts or debugging `println!`s), you can connect like this:

```
target remote | cargo v5 t | tee out.log
```

## Licensing

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed under the MIT and Apache-2.0 licenses, without any additional terms or conditions.
