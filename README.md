# v5gdb: VEX V5 debugger

> *"One day we will have all the features of ROBOTC on the V5 brain" - Charles, 2114V*

v5gdb is a debugger backend for the VEX V5 platform that is compatible with GDB. It uses a combination of hardware and software breakpoints to implement features like line-by-line stepping and user-defined breakpoints. Users are also given the ability to read program state such as the arguments passed to the active function or global variables.

## Getting Started

Please read the [user manual](https://github.com/vexide/v5gdb/wiki)!

## Goals

- Implement a debug server usable by an instance of `gdb`.
- Support multiple methods of communication with a debugger client running on the user's PC.
- Implement non-invasive breakpoint handling compatible with all existing VEX runtimes.
- Remote management of dynamically-placed software breakpoints
- Single-stepping and hardware breakpoints
- On-device debugger setup via Rust and C APIs
- Easy to add to existing VEX projects, including vexide, PROS, and VEXcode projects.

## Design

This project implements a debug server that can communicate with a GDB-compatible client over various transport methods. For more information, see the project's [wiki page](https://internals.vexide.dev/technical/debugger).

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
