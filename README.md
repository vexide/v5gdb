# v5gdb: VEX V5 debugger

> *"One day we will have all the features of ROBOTC on the V5 brain" - Charles, 2114V*

v5gdb is a debugger backend for the VEX V5 platform that is compatible with GDB. It uses a combination of hardware and software breakpoints to implement features like line-by-line stepping and user-defined breakpoints. Users are also given the ability to read program state such as the arguments passed to the active function or global variables.

## Getting Started

Please read the [user manual] for detailed installation and usage instructions!

[user manual]: https://github.com/vexide/v5gdb/wiki

### Rust (vexide)

```rust
use v5gdb::prelude::*;

#[vexide::main]
async fn main(_peripherals: Peripherals) {
    v5gdb::install(V5Debugger::stdio().stop_motors_on_break(true));
}
```

### C++ (PROS)

```cpp
#include "v5gdb/v5gdb.h"

void initialize() {
    v5gdb::install(v5gdb::StdioTransport(), {.stop_motors_on_break = true});
}
```

Connect from your computer with `v5gdb path/to/program.elf` (the ELF path is optional in PROS projects).

## Features

Here are the features of v5gdb that you can use today:

- Connect to your VEX V5 robot using GDB Remote Debugging
- Easily customize how v5gdb communicates with your PC
- Fully compatible with vexide and PROS frameworks
- Remotely view what's stored in variables and manage dynamically-placed software/hardware breakpoints
- Single-step through instructions and lines of code
- Pause your program in arbitrary locations by pressing Ctrl-C
- Simple to configure and enable via Rust and C++ APIs
- Basic support for thread-aware debugging (requires FreeRTOS)

## Future Goals

Here are the features of v5gdb that aren't done yet or are planned for the future:

- Easy to add to existing projects via a Rust crate on Crates.io and PROS template depot (for now, manually build v5gdb using the instructions in the [user manual])
- Compatibility with VEXcode framework (right now it crashes)
  - TBD on whether thread-aware debugging is possible for VEXcode

## Design

This project implements a debug server that can communicate with a GDB-compatible client over various transport methods. For more information, see the project's [internals wiki page](https://internals.vexide.dev/technical/debugger).

## License

Licensed under either of

* Apache License, Version 2.0

  ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license

  ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

## Contribution

For guidelines for contributors, see [CONTRIBUTING.md](CONTRIBUTING.md).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
