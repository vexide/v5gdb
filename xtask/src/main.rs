use std::{
    env,
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Read, Write, stderr},
    path::Path,
    process::{Command, Stdio, exit},
    time::Duration,
};

use clap::Parser;
use indicatif::ProgressBar;

#[derive(Debug, Parser)]
enum Args {
    Test,
    /// Build v5gdb as a static library for FFI.
    Build {
        target: FfiTarget,
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "CARGO-OPTIONS"
        )]
        extra_args: Vec<String>,
    },
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum FfiTarget {
    Pros,
    Vexcode,
}

fn main() {
    let args = Args::parse();
    match args {
        Args::Test => test(),
        Args::Build { target, extra_args } => build(target, extra_args),
    }
}

fn cargo() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn test() {
    let mut locate_cmd = Command::new(cargo());
    locate_cmd.args(["locate-project", "--message-format=plain", "--workspace"]);
    let out = locate_cmd.output().unwrap();
    let cargo_toml = Path::new(std::str::from_utf8(&out.stdout).unwrap().trim());
    let tests_dir = cargo_toml.join("../tests").canonicalize().unwrap();

    for file in tests_dir.read_dir().unwrap() {
        let file = file.unwrap();
        let path = file.path();
        let ext = path.extension();
        if ext != Some(OsStr::new("rs")) {
            continue;
        }

        let test_name = path.file_prefix().unwrap();
        println!("Running test {test_name:?}");

        let progress =
            ProgressBar::new_spinner().with_message(format!("Test uploading - {test_name:?}"));

        progress.enable_steady_tick(Duration::from_millis(250));

        let mut test_command = Command::new(cargo());
        test_command.args(["v5", "run", "-p=v5gdb", "--test"]);
        test_command.arg(test_name);
        test_command.stdout(Stdio::piped());
        test_command.stderr(Stdio::piped());

        let mut process = test_command.spawn().unwrap();
        let stdout = process.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        let mut error = true;

        for line in reader.lines() {
            let line = line.unwrap();

            if let Some(directive) = line.trim().strip_prefix("::test:")
                && let Some((cmd, arg)) = directive.split_once('=')
                && cmd == "status"
            {
                match arg {
                    "run" => progress.set_message(format!("Test running - {test_name:?}")),
                    "done" => {
                        progress.set_message(format!("Test succeeded - {test_name:?}"));
                        error = false;
                        break;
                    }
                    "error" => {
                        progress.set_message(format!("Test errored - {test_name:?}"));
                        break;
                    }
                    "panic" => {
                        progress.set_message(format!("Test panicked - {test_name:?}"));
                        break;
                    }
                    _ => {}
                }
            } else {
                progress.println(format!("{line}"));
            }
        }

        progress.finish();

        if let Some(status) = process.try_wait().unwrap()
            && !status.success()
        {
            break;
        }

        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{self, Signal},
                unistd::Pid,
            };

            let pid = Pid::from_raw(process.id() as i32);
            signal::kill(pid, Signal::SIGINT).expect("Failed to send SIGINT");
        }
        #[cfg(not(unix))]
        {
            process.kill().unwrap();
        }

        _ = process.wait();

        if error {
            let mut stderr = stderr();
            let mut err_out = process.stderr.take().unwrap();
            let mut err_buf = Vec::new();
            err_out.read_to_end(&mut err_buf).unwrap();
            stderr.write_all(&err_buf).unwrap();
        }
    }
}

fn build(target: FfiTarget, opts: Vec<String>) {
    let target_args: &[&str] = match target {
        // Normal hard-float build, but avoid building std to prevent accidentally using the wrong
        // allocator. Rust's std port will try to manage the heap, but PROS is already doing that.
        FfiTarget::Pros => &[
            "--target=armv7a-vex-v5",
            "-Zbuild-std=core",
            "-Fv5gdb/pros",
        ],
        FfiTarget::Vexcode => &[
            "--target=armv7a-none-eabi",
            "-Zbuild-std=core",
        ],
    };

    let mut cargo = Command::new(cargo());
    cargo.args(["build", "-p=v5gdb-ffi"]);
    cargo.args(target_args);
    cargo.args(&opts); // e.g. --release

    let mut child = cargo.spawn().expect("Failed to start cargo");
    let code = child.wait().unwrap_or_default();
    exit(code.code().unwrap_or(1));
}
