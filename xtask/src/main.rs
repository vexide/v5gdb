use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{BufRead, BufReader, Read, Write, stderr},
    path::{Path, PathBuf},
    process::{Command, Stdio, exit},
    time::Duration,
};

use cargo_metadata::Message;
use clap::Parser;
use fs_extra::dir::{CopyOptions, get_dir_content};
use indicatif::ProgressBar;
use serde_json::{Value, json};

#[derive(Debug, Parser)]
enum Args {
    Test,
    /// Build v5gdb as a static library for FFI.
    ///
    /// If the compilation target is "pros", a PROS template containing v5gdb will be built and its
    /// path will be printed to the terminal.
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

#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
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

/// The `firmware` workspace containing v5gdb and other on-device code.
fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../firmware"))
}

fn test() {
    let tests_dir = firmware_dir().join("tests").canonicalize().unwrap();

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
        test_command.current_dir(firmware_dir());
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
                progress.println(&line);
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
        FfiTarget::Pros => &[
            // Use vex-v5 instead of none-eabihf so that extern "System" functions resolve to the
            // soft-float AAPCS ABI that VEXos uses.
            "--target=armv7a-vex-v5",
            // HACK(PROS): PROS programs hang when v5gdb is built for Thumb. (#25)
            r#"--config=target."cfg(true)".rustflags=["-Ctarget-feature=-thumb-mode"]"#,
            // Avoid building std to prevent accidentally using the wrong allocator. Rust's std
            // port will try to manage the heap, but PROS is already doing that.
            "-Zbuild-std=core",
            "-Fv5gdb/pros",
        ],
        // A soft-float target is used to match VEXcode's ABI behavior.
        FfiTarget::Vexcode => &["--target=armv7a-none-eabi", "-Zbuild-std=core"],
    };

    let mut cargo = Command::new(cargo());
    cargo.current_dir(firmware_dir());
    cargo.args([
        "build",
        "-p=v5gdb-ffi",
        "--message-format=json-render-diagnostics",
    ]);
    cargo.args(target_args);
    cargo.args(&opts); // e.g. --release
    cargo.stdout(Stdio::piped());

    let mut child = cargo.spawn().expect("Failed to start cargo");
    let reader = BufReader::new(child.stdout.take().unwrap());

    // Look for staticlib build outputs
    let mut library_path = None;
    for message in Message::parse_stream(reader) {
        if let Message::CompilerArtifact(artifact) = message.unwrap()
            && artifact.target.is_staticlib()
            && artifact.target.name == "v5gdb"
        {
            library_path = Some(artifact.filenames[0].clone());
        }
    }

    let status = child.wait().unwrap_or_default();
    if !status.success() {
        exit(status.code().unwrap_or(1));
    }

    // Package pros builds if the build produced a staticlib output
    if target == FfiTarget::Pros
        && let Some(library_path) = library_path
    {
        make_pros_template(library_path.as_path().as_std_path());
    }
}

fn make_pros_template(library: &Path) {
    let cargo_manifest =
        fs::read(firmware_dir().join("Cargo.toml")).expect("v5gdb Cargo.toml exists");
    let cargo_manifest: Value =
        toml::from_slice(&cargo_manifest).expect("v5gdb Cargo.toml is valid");

    let package_version = cargo_manifest["package"]["version"].as_str().unwrap();

    let target_dir = library.parent().unwrap();
    let template_staging_dir = target_dir.join("pros-template");

    let template_id = format!("v5gdb@{package_version}");
    let template_dir = template_staging_dir.join(&template_id);

    _ = fs::remove_dir_all(&template_staging_dir);
    fs::create_dir_all(&template_dir).unwrap();

    // Copy includes and config files to template
    let ffi_dist_dir = firmware_dir().join("ffi/dist");
    fs_extra::dir::copy(
        &ffi_dist_dir,
        &template_dir,
        &CopyOptions {
            overwrite: true,
            content_only: true,
            ..Default::default()
        },
    )
    .unwrap();

    // Copy artifact to template
    let firmware_dir = template_dir.join("firmware");
    let library_name = library.file_name().unwrap();

    _ = fs::create_dir(&firmware_dir);
    fs::copy(library, firmware_dir.join(library_name)).unwrap();

    // Enumerate template contents and create the manifest.
    let contents = get_dir_content(&template_dir).unwrap();
    let files = contents
        .files
        .iter()
        .map(Path::new)
        .map(|p| p.strip_prefix(&template_dir).unwrap().to_path_buf())
        .collect::<Vec<_>>();

    let manifest = make_pros_manifest(&cargo_manifest, &files);
    fs::write(template_dir.join("template.pros"), manifest).unwrap();

    // Finally, zip the whole thing.
    let zip_path = template_staging_dir.join(format!("{template_id}.zip"));
    let mut zip_cmd = Command::new("zip")
        .args(["-r", "-9"])
        .arg(&zip_path)
        .arg(".")
        .current_dir(&template_dir)
        .spawn()
        .unwrap();

    let zip_status = zip_cmd.wait().unwrap();
    if !zip_status.success() {
        exit(zip_status.code().unwrap_or(1));
    }

    let relative_zip_file = zip_path
        .strip_prefix(env::current_dir().unwrap())
        .unwrap_or(&zip_path);

    println!("Built template: {}\n", zip_path.display());

    println!("To register this template with PROS, please run:\n");
    println!("\tpros c fetch {}", relative_zip_file.display());

    println!("\nOnce registered, install it in a project with:\n");
    println!("\tpros c apply v5gdb --force-apply");
}

fn make_pros_manifest(cargo_manifest: &Value, files: &[PathBuf]) -> Vec<u8> {
    let pros_metadata = &cargo_manifest["package"]["metadata"]["pros"];

    let pros_manifest = json!({
        "metadata": pros_metadata["metadata"],
        "name": cargo_manifest["package"]["name"],
        "supported_kernels": pros_metadata["supported_kernels"],
        "target": pros_metadata["target"],
        "system_files": files,
        "user_files": [],
        "version": cargo_manifest["package"]["version"],
    });

    serde_json::to_vec(&json!({
        "py/object": "pros.conductor.templates.external_template.ExternalTemplate",
        "py/state": pros_manifest,
    }))
    .expect("manifest is valid")
}
