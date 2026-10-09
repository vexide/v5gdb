use core::{iter, str::FromStr};

use gdbstub::target::ext::monitor_cmd::{ConsoleOutput, MonitorCmd};
use vex_sdk::*;

use crate::{
    gdb_target::V5Target,
    logging::{self, LevelFilter},
    sdk::{competition, stop_all_motors},
    sys::{DebuggerSystem, System},
};

const MONITOR_DESCRIPTION: &str =
    concat!("v5gdb debug server, version ", env!("CARGO_PKG_VERSION"));

const HELP_MSG: &str = "
Monitor commands:
    help                                Show this help message.
    stop                                Immediately stop all motors right now.
    autostop ?                          Show whether auto-stop on breakpoint is enabled.
    autostop (true | false)             Enable or disable automatic motor stop on breakpoint.
    dev                                 List connected smart devices.
    batt                                View battery status.
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
            // `monitor stop`
            // stop all motors right now, regardless of the
            // auto-stop setting.
            "stop" => {
                stop_all_motors();
                gdbstub::outputln!(out, "All motors stopped.");
            }

            // `monitor autostop [true | false]`
            //
            // with no args:        tells the user what commands they can use.
            // with "true":         enable auto-stop on every breakpoint.
            // with "false":        disable auto-stop.
            // with "?"             prints the state of auto-stop.
            "autostop" => match args.next() {
                Some("true") => {
                    self.stop_motors_on_break = true;
                    gdbstub::outputln!(
                        out,
                        "auto motor-stop on breakpoint: ENABLED.\n\
                             All motors will be stopped immediately whenever a breakpoint fires."
                    );
                }
                Some("false") => {
                    self.stop_motors_on_break = false;
                    gdbstub::outputln!(out, "Auto motor-stop on breakpoint: DISABLED.");
                }
                Some("?") => {
                    let state = if self.stop_motors_on_break {
                        "ENABLED"
                    } else {
                        "DISABLED"
                    };

                    gdbstub::outputln!(
                        out,
                        "Auto motor-stop on breakpoint: {state}.\n\
                             Use `monitor autostop true` or `monitor autostop false` to change the state."
                    );
                }
                Some(unknown) => {
                    gdbstub::outputln!(
                        out,
                        "Unknown argument '{unknown}'. Use 'true' or 'false' or '?'.\n\
                             Example: `monitor autostop true`"
                    );
                }
                None => {
                    gdbstub::outputln!(
                        out,
                        "Please include valid arguments this includes: \n\
                             `autostop ?`                Show whether auto-stop on breakpoint is enabled.\n\
                             `autostop (true | false)`   Enable or disable automatic motor stop on breakpoint."
                    );
                }
            },

            "dev" | "devices" => devices(&mut out),
            "batt" | "battery" => battery(&mut out),
            "ctrl" => controller(args.next(), &mut out),
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
                    logging::set_max_level(level);
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

/// The controller buttons reported by `monitor ctrl`.
const CONTROLLER_BUTTONS: &[(&str, V5_ControllerIndex)] = &[
    ("L1", V5_ControllerIndex::ButtonL1),
    ("L2", V5_ControllerIndex::ButtonL2),
    ("R1", V5_ControllerIndex::ButtonR1),
    ("R2", V5_ControllerIndex::ButtonR2),
    ("Up", V5_ControllerIndex::ButtonUp),
    ("Down", V5_ControllerIndex::ButtonDown),
    ("Left", V5_ControllerIndex::ButtonLeft),
    ("Right", V5_ControllerIndex::ButtonRight),
    ("X", V5_ControllerIndex::ButtonX),
    ("B", V5_ControllerIndex::ButtonB),
    ("Y", V5_ControllerIndex::ButtonY),
    ("A", V5_ControllerIndex::ButtonA),
    ("SEL", V5_ControllerIndex::ButtonSEL),
];

fn controller(which: Option<&str>, out: &mut ConsoleOutput<'_>) {
    let (id, label) = match which {
        Some("partner" | "p") => (V5_ControllerId::kControllerPartner, "Partner"),
        _ => (V5_ControllerId::kControllerMaster, "Primary"),
    };

    // SAFETY: The controller SDK calls only read state and are safe to call while halted.
    let connection = match unsafe { vexControllerConnectionStatusGet(id) } {
        V5_ControllerStatus::kV5ControllerTethered => "Tethered",
        V5_ControllerStatus::kV5ControllerVexnet => "VEXnet",
        _ => {
            gdbstub::outputln!(out, "{label} controller: offline");
            return;
        }
    };

    let get = |index| unsafe { vexControllerGet(id, index) };

    gdbstub::outputln!(out, "{label} controller ({connection}):");
    gdbstub::outputln!(
        out,
        "  left stick:  ({:>4}, {:>4})",
        get(V5_ControllerIndex::AnaLeftX),
        get(V5_ControllerIndex::AnaLeftY),
    );
    gdbstub::outputln!(
        out,
        "  right stick: ({:>4}, {:>4})",
        get(V5_ControllerIndex::AnaRightX),
        get(V5_ControllerIndex::AnaRightY),
    );
    gdbstub::outputln!(
        out,
        "  battery:     {}%",
        get(V5_ControllerIndex::BatteryCapacity),
    );

    gdbstub::output!(out, "  active buttons: ");
    let mut any_pressed = false;
    for &(name, index) in CONTROLLER_BUTTONS {
        if get(index) != 0 {
            gdbstub::output!(out, " {name}");
            any_pressed = true;
        }
    }
    if !any_pressed {
        gdbstub::output!(out, " (none)");
    }
    gdbstub::outputln!(out);
}

fn devices(out: &mut ConsoleOutput<'_>) {
    let mut types = [V5_DeviceType::kDeviceTypeUndefinedSensor; V5_MAX_DEVICE_PORTS];
    // SAFETY: `types` is a valid buffer of exactly `V5_MAX_DEVICE_PORTS` elements.
    unsafe {
        vexDeviceGetStatus(types.as_mut_ptr());
    }

    let mut found = false;
    for (i, &ty) in types.iter().enumerate() {
        if ty == V5_DeviceType::kDeviceTypeUndefinedSensor {
            break;
        }

        if ty == V5_DeviceType::kDeviceTypeNoSensor {
            continue;
        }

        found = true;
        gdbstub::outputln!(out, "  port {:>2}: {}", i + 1, device_type_name(ty));
    }
    if !found {
        gdbstub::outputln!(out, "No devices connected.");
    }
}

/// A human-readable name for a smart device type.
fn device_type_name(ty: V5_DeviceType) -> &'static str {
    match ty {
        V5_DeviceType::kDeviceTypeMotorSensor => "Motor",
        V5_DeviceType::kDeviceTypeAbsEncSensor => "Rotation",
        V5_DeviceType::kDeviceTypeImuSensor => "Inertial (IMU)",
        V5_DeviceType::kDeviceTypeDistanceSensor => "Distance",
        V5_DeviceType::kDeviceTypeRadioSensor => "Radio",
        V5_DeviceType::kDeviceTypeTetherSensor => "Controller",
        V5_DeviceType::kDeviceTypeBrainSensor => "Brain",
        V5_DeviceType::kDeviceTypeVisionSensor => "Vision",
        V5_DeviceType::kDeviceTypeAdiSensor => "ADI (3-Wire)",
        V5_DeviceType::kDeviceTypeOpticalSensor => "Optical",
        V5_DeviceType::kDeviceTypeMagnetSensor => "Electromagnet",
        V5_DeviceType::kDeviceTypeGpsSensor => "GPS",
        V5_DeviceType::kDeviceTypeAiVisionSensor => "AI Vision",
        V5_DeviceType::kDeviceTypeLightTowerSensor => "Light Tower",
        V5_DeviceType::kDeviceTypeArmDevice => "Workcell Arm",
        V5_DeviceType::kDeviceTypePneumaticSensor => "Pneumatics",
        V5_DeviceType::kDeviceTypeGenericSerial => "Generic Serial",
        _ => "Unknown",
    }
}

fn battery(out: &mut ConsoleOutput<'_>) {
    // SAFETY: The battery SDK calls only read telemetry.
    let (millivolts, milliamps, temperature, capacity) = unsafe {
        (
            vexBatteryVoltageGet(),
            vexBatteryCurrentGet(),
            vexBatteryTemperatureGet(),
            vexBatteryCapacityGet(),
        )
    };

    gdbstub::outputln!(out, "voltage:     {millivolts} mV");
    gdbstub::outputln!(out, "current:     {milliamps} mA");
    gdbstub::outputln!(out, "temperature: {temperature} \u{b0}C");
    gdbstub::outputln!(out, "capacity:    {capacity}%");
}