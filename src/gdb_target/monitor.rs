use core::{iter, str::FromStr};

use gdbstub::target::ext::monitor_cmd::{ConsoleOutput, MonitorCmd};
use log::LevelFilter;
use vex_sdk::*;

use crate::{
    gdb_target::V5Target,
    sdk::competition,
    sys::{DebuggerSystem, System},
};

const MONITOR_DESCRIPTION: &str =
    concat!("v5gdb debug server, version ", env!("CARGO_PKG_VERSION"),);

const HELP_MSG: &str = "
Monitor commands:
    help                                Show this help message.
    stop                                Stop all motors.
    ctrl [partner]                      View controller state, primary (default) or partner.
    comp                                View competition state.
    comp (driver | auton | disabled)    Override competition mode.
    comp (none | fc | switch)           Override competition system.
    comp real                           Stop overriding competition state.
    log [level]                         Set the current log level (off/trace/debug/info/warn/error).
    dbg break                           Show internal software breakpoint status.
    dbg hw                              Show internal hardware debug status.
";

impl MonitorCmd for V5Target {
    fn handle_monitor_cmd(
        &mut self,
        data: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let cmd_str = str::from_utf8(data).unwrap_or_default();

        let mut args = cmd_str.split_whitespace();
        let cmd = args.next().unwrap_or("help");

        match cmd {
            "stop" => {
                for port_num in 0..V5_MAX_DEVICE_PORTS {
                    unsafe {
                        let device = vexDeviceGetByIndex(port_num as u32);
                        if device.is_null() {
                            break;
                        }
                        vexDeviceMotorVoltageSet(device, 0);
                    }
                }
            }
            "ctrl" => gdbstub::outputln!(out, "Unimplemented"), // TODO
            "comp" => {
                let change = args.next();
                match change {
                    Some("driver" | "op" | "opcontrol") => {
                        let status = competition::read_status()
                            .with_disabled(false)
                            .with_autonomous(false);
                        competition::set_override(Some(status));
                    }
                    Some("auto" | "auton" | "autonomous") => {
                        let status = competition::read_status()
                            .with_disabled(false)
                            .with_autonomous(true);
                        competition::set_override(Some(status));
                    }
                    Some("dis" | "disabled") => {
                        let status = competition::read_status()
                            .with_disabled(true)
                            .with_autonomous(false);
                        competition::set_override(Some(status));
                    }
                    Some("none" | "disconnected") => {
                        let status = competition::read_status()
                            .with_connected(false)
                            .with_system(false);
                        competition::set_override(Some(status));
                    }
                    Some("fc" | "field-control") => {
                        let status = competition::read_status()
                            .with_connected(true)
                            .with_system(true);
                        competition::set_override(Some(status));
                    }
                    Some("switch") => {
                        let status = competition::read_status()
                            .with_connected(true)
                            .with_system(false);
                        competition::set_override(Some(status));
                    }
                    Some("real") => {
                        competition::set_override(None);
                    }
                    Some(_) => gdbstub::outputln!(out, "Unknown competition state type"),
                    None => {
                        let real = competition::read_real_status();
                        let overridden = competition::read_override();

                        if let Some(overridden) = overridden {
                            gdbstub::outputln!(out, "override: {overridden:?}");
                            gdbstub::outputln!(out, "real: {real:?}");
                        } else {
                            gdbstub::outputln!(out, "status: {real:?}");
                        }
                    }
                }
            }
            "dbg" => {
                let Some(subcommand) = args.next() else {
                    gdbstub::outputln!(out, "Please specify a subcommand.");
                    return Ok(());
                };

                match subcommand {
                    "break" => {
                        for (i, breakpt) in self.breaks.iter().enumerate() {
                            gdbstub::outputln!(out, "{i:>2}: {breakpt:x?}");
                        }
                    }
                    "hw" => {
                        gdbstub::outputln!(out, "{:#x?}", self.hw_manager);
                    }
                    _ => {
                        gdbstub::outputln!(
                            out,
                            "Unknown subcommand. See 'monitor help' for more info."
                        );
                    }
                }
            }
            "log" => {
                if let Some(level) = args.next()
                    && let Ok(level) = LevelFilter::from_str(level)
                {
                    log::set_max_level(level);
                } else {
                    gdbstub::outputln!(out, "Expected off/trace/debug/info/warn/error.")
                }
            }
            "sys" => {
                System::handle_monitor_cmd(args, &mut out);
            }
            "?" | "h" | "help" => {
                gdbstub::outputln!(out, "{MONITOR_DESCRIPTION}\n{HELP_MSG}");
                System::handle_monitor_cmd(iter::once("help"), &mut out);
            }
            _ => {
                gdbstub::outputln!(out, "Unknown command. See 'monitor help' for more info.");
            }
        }

        Ok(())
    }
}
