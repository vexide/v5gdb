# FFI Bindings for v5gdb

v5gdb is written in Rust, but it can be used from C++ using the bindings in this crate.

## Instructions

Building this crate will create a compilation artifact named "libv5gdb.a" which can be added into
a C++ project in the normal way (whatever that means for you). The headers for this library are
located in the `include` directory in this folder.

Run this from the `firmware` directory:

```sh
cargo build -p v5gdb-ffi --target=armv7a-vex-v5 -Zbuild-std=core -Fv5gdb/pros
ls ./target/armv7a-vex-v5/debug/libv5gdb.a
```

Building with `--release`/`-r` will reduce the file size. All release builds have debug info
enabled (see `profile.release` in `firmware/Cargo.toml`).

### Command-line arguments

Depending on your needs, you should pass different arguments to Cargo when building. Here are some
of the Rust configuration flags which you might find useful.

* `-Z build-std=core` should be passed if you don't want to use the Rust allocator (which is likely
  if your project is written in C++).
* `--target armv7a-vex-v5` should be passed to perform a build for VEX V5 with the hard-float ABI.
* `--target armv7a-none-eabi` should be passed to perform a build for VEX V5 with the soft-float
  ABI.

Here are some flags that configure v5gdb's optional features. These are all defined in
`firmware/Cargo.toml`.

* `-F v5gdb/freertos` should be passed if you want to enable v5gdb's FreeRTOS integration.
* `-F v5gdb/pros` should be passed if you want to enable v5gdb's FreeRTOS integration with
  additional handling for the quirks of the FreeRTOS port included in the PROS kernel.

### Framework support

If you are using a common C++ framework (currently only PROS is supported), you can use the
`cargo xtask build <framework>` subcommand which will automatically set the correct command line
flags and do other useful things. Run it from the repository root. The code for this subcommand lives in the
`xtask` folder at the repository root.

* `cargo xtask build pros [--release]`:

  Build for the PROS framework, and also create a PROS Conductor `.zip` template file for
  distribution.
