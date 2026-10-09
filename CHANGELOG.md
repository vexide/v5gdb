# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The new `v5gdb::logging` module allows configuring the debugger's logger. (#46)

### Changed

- Debugger log messages are now printed directly to serial instead of through the global `log` logger. (**Breaking change**) (#46)

### Fixed

- Fixed a memory safety issue which corrupted FreeRTOS task state when resuming execution. (#42)
- User IRQ handlers are now skipped when running them could corrupt state, including during single steps. (#42)
- Triggering a breakpoint from an unsupported CPU mode now causes a panic. (#42)
- Triggering a breakpoint while another breakpoint is still active now causes a panic. (#42)
- Recursively entering the debug monitor now panics instead of locking up the system. (#44)
- Software breakpoint addresses are now validates properly. (#41)
- The FreeRTOS monitor help now says "Stack Remaining" is measured in words, not bytes.

### New Contributors

- @ion098 made their first contribution in #41!

## [0.1.0-alpha.2]

### Added

- Motors can now be automatically stopped when a breakpoint is triggered. (#22)
- Pressing Ctrl-C now pauses the running program. (#24)
- GDB can now disconnect and reconnect without restarting the session.
- Added new monitor commands including `dev`, `batt`, and `ctrl`.
- Thumb SDKs calls can now be intercepted correctly by v5gdb
- Improved the representation of errors in FFI bindings

### Fixed

- PROS builds now use the correct SDK ABI.
- Fixed invalid alignment assumptions when reading the FreeRTOS FPU context.
- Significantly reduced stack usage to avoid stack overflows.
- Single-register access now returns the value from the correct register.
- Fixed handling of Thumb breakpoints and exit requests.
- Significantly improved the robustness of the default `StdioTransport`.
- Improved selection of system GDB binaries.
- Fixed crash on FreeRTOS tasks with null names.

## [0.1.0-alpha.1]

### Added

- Debug monitor for after a breakpoint is hit.
    - Support for managing breakpoints
    - Support for reading/writing arbitrary memory and registers
- Implementation of GDB protocol
- Debug exception setup and handling
- Plain USB serial transport method


[0.1.0-alpha.1]: https://github.com/vexide/v5gdb/compare/160aa901a471ab20d139c07d9adc3479c8823dea...v0.1.0-alpha.1
[0.1.0-alpha.2]: https://github.com/vexide/v5gdb/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
