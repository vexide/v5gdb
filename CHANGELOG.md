# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- The v5gdb CLI will now search the user's PROS toolchain when looking for suitable GDB installations.
- Added the `-c`/`--client`/`$V5GDB_CLIENT` flag for customizing which GDB executable is used.
- Added the `-C`/`--client-arg` flag (repeatable) for customizing what flags are passed to GDB.
- Added the `--lldb` flag for debugging with LLDB instead of GDB. It's turned on automatically
  when the `--client` executable's name contains "lldb".

### Fixed

- Hardware breakpoints can now be removed when the client sends a different breakpoint kind than
  it used to add them. LLDB does this for Thumb breakpoints.

## [0.1.0-beta.1]

### Added

- C++ users may now specify a debugger config as an optional parameter to `v5gdb::install`. (#58)
- v5gdb can now attach to running programs without a call to `breakpoint!()`. (#51)
- v5gdb's log level may now be specified in the debugger config. (#58)
- v5gdb can now retransmit its packets to GDB when they are lost/corrupted. (#56)
- v5gdb will now ask GDB to retransmit its packets when they are corrupted. (#56)
- Most fatal debugger errors will no longer panic and now only require re-connecting GDB. (#56)
- Added `V5Debugger::stdio()` as shorthand for `V5Debugger::new(StdioTransport::new())`. (#58)
- Added `is_installed()` to get whether the debugger is installed. (#58)
- Added the `install_stdio()` function as shorthand for installing a `V5Debugger::stdio()`. (#58)
- The CLI now prints a message after entering TCP Server mode.

### Changed

- `breakpoint!()` now does nothing instead of crashing when the debugger hasn't been installed. (#58)
- TransportError is now opaque. (#58) (**Breaking Change**)
- The `gdbstub` Connection/ConnectionExt traits have been merged with v5gdb's Transport trait. (#58) (**Breaking Change**)
- `Debugger::handle_debug_event` now returns an enum instead of a bool. (#58) (**Breaking Change**)
- `V5Debugger::new` no longer has side effects; they now happen at install-time. (#58)
- The `Debugger` trait no longer requires the Any trait. (#58)
- v5gdb will now check for stack overflows before resuming user code.
- Significantly increased performance by avoiding unneeded serial packets. (#54)

### Fixed

- v5gdb will now panic if compiled with an unsupported LTO configuration. (#57)
- The GIC interrupt mask is now saved and restored; this fixes stepping through PROS critical sections.
- v5gdb should no longer overflow the debugger stack in debug builds.
- Fixed CLI compilation on Windows.
- Stopped the characters "d+" from sometimes being printed when v5gdb is first started.
- Fixed prints from before v5gdb is installed not appearing consistently.

### Removed

- Removed stubbed implementations for non-VEXos platforms. (#58) (**Breaking Change**)
- Certain implementation details are no longer publicly exported. (#58) (**Breaking Change**)

## [0.1.0-alpha.3]

### Added

- The new `v5gdb::logging` module allows configuring the debugger's logger. (#46)
- GDB scheduler locking is now partially supported (`set scheduler-locking on`). (#48)
- The v5gdb CLI will now auto-detect the user's framework and use it to customize default behavior. (#49)
- Running v5gdb without specifying any ELF files will now default to the most recent build output in a PROS project. (#49)
- Added support for the PROS serial protocol in the v5gdb CLI. (#49)
- The serial protocol used by the v5gdb CLI can now be customized with `--user-io` if the correct protocol is not automatically chosen. (#49)
- Logging in the v5gdb CLI can now be customized with the `V5GDB_LOG` environment variable. (#49)

### Changed

- Debugger log messages are now printed directly to serial instead of through the global `log` logger. (**Breaking change**) (#46)
- The `v5gdb::install` function no longer requires an argument when called from C++.
- The `v5gdb::install` function now takes an rvalue in C++. (**Breaking change**)
- The function signatures for custom transports in C++ have been slightly tweaked. (**Breaking change**)

### Fixed

- Fixed a memory safety issue which corrupted FreeRTOS task state when resuming execution. (#42)
- User IRQ handlers are now skipped when running them could corrupt state, including during single steps. (#42)
- Triggering a breakpoint from an unsupported CPU mode now causes a panic. (#42)
- Triggering a breakpoint while another breakpoint is still active now causes a panic. (#42)
- Recursively entering the debug monitor now panics instead of locking up the system. (#44)
- Software breakpoint addresses are now validates properly. (#41)
- The FreeRTOS monitor help now says "Stack Remaining" is measured in words, not bytes.
- Unsupported resume configurations will now print an error instead of crashing. (#48)
- v5gdb now reports the actual current thread to GDB instead of the highest priority one. (#48)

### New Contributors

- @speedskater1610 made their first contribution in #22!
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
[0.1.0-alpha.3]: https://github.com/vexide/v5gdb/compare/v0.1.0-alpha.2...v0.1.0-alpha.3
[0.1.0-beta.1]: https://github.com/vexide/v5gdb/compare/v0.1.0-alpha.3...v0.1.0-beta.1
