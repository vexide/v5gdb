use std::{
    cell::LazyCell, env::current_dir, ffi::OsString, path::PathBuf, process::exit, time::Duration,
};

use clap::Parser;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, stderr},
    net::TcpListener,
};
use tracing::{debug, error, trace, warn};
use tracing_subscriber::EnvFilter;
use vex_v5_serial::{
    Connection,
    serial::{SerialDevice, find_devices},
};

use crate::{
    debugger::Debugger,
    framework::{Project, ProjectKind},
    serial::{ErrorHandler, SerialStreamError, V5SerialStream, pros::ProsDecoder},
};

mod debugger;
mod framework;
mod serial;

/// A format which can be used to decode user serial output.
#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
enum UserIoMode {
    Auto,
    Pros,
    Raw,
}

/// Use GDB to debug a VEX V5 robot running the v5gdb debug server.
///
/// This command interrupts the program on connect, so it can attach to robots that are running or
/// already stopped at a breakpoint. Use the V5GDB_LOG environment variable to enable internal CLI
/// logging. On-device logging can be controlled using GDB monitor commands.
#[derive(Debug, clap::Parser)]
struct Args {
    /// Instead of starting GDB as a subprocess, open a GDB remote server on the given address.
    #[clap(long)]
    tcp: Option<String>,
    /// List of ELF files to debug with GDB (has no effect in TCP mode).
    elf_files_to_debug: Vec<PathBuf>,
    /// Logs all decoding errors and serial I/O (same as adding `v5gdb::serial=trace` to
    /// `V5GDB_LOG`).
    #[clap(long)]
    debug_io: bool,
    /// Disables auto-discovery of ELF files when none are specified.
    #[clap(long, env = "V5GDB_NO_DISCOVER_ELF")]
    no_discover_elf: bool,
    /// Controls the format which is used to parse user serial output.
    ///
    /// auto: The format to use is inferred from the current directory.
    ///
    /// pros: Program output is decoded using the PROS serial wire format.
    ///
    /// raw: No extra processing is applied.
    #[clap(long, default_value = "auto", env = "V5GDB_USER_IO")]
    user_io: UserIoMode,
    /// The debug client to use. It must accept GDB's command-line arguments. By default, v5gdb
    /// searches for a suitable GDB executable in various locations.
    #[clap(short, long, env = "V5GDB_CLIENT")]
    client: Option<PathBuf>,
    /// Extra arguments to pass to the debug client.
    #[clap(short = 'C', long = "client-arg")]
    client_args: Vec<OsString>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = Args::parse();
    init_logging(args.debug_io)?;

    let cwd = current_dir()?;
    let project = LazyCell::new(|| {
        debug!("Looking for a project directory");
        Project::discover(&cwd).ok().flatten()
    });

    debug!("Scanning for VEX V5 devices to connect to...");
    let mut all_devices = find_devices()?;
    if all_devices.is_empty() {
        error!("No V5 device is connected");
        exit(1);
    }
    let device = all_devices.remove(0);
    debug!(device = ?device, "We'll use this device");

    if args.user_io == UserIoMode::Auto {
        if let Some(project) = &*project
            && project.kind() == ProjectKind::Pros
        {
            args.user_io = UserIoMode::Pros;
        } else {
            args.user_io = UserIoMode::Raw;
        }
        debug!(resolved = ?args.user_io, "Resolved 'Auto' user I/O mode");
    }

    let addr = args.tcp.as_deref().unwrap_or("127.0.0.1:35537");
    let server = TcpListener::bind(addr).await?;
    if args.tcp.is_some() {
        // This allows VS Code and others to know when we're ready to connect.
        eprintln!("Listening on {}", server.local_addr()?);
    }
    let server_task = tokio::spawn(async move {
        serve_device_serial(device, server, args.user_io)
            .await
            .unwrap()
    });

    if args.tcp.is_some() {
        debug!("TCP mode is on: won't spawn a debugger subprocess");
        if args.client.is_some() || !args.client_args.is_empty() {
            warn!(
                "--client/--client-arg was specified, but these don't do anything when --tcp is \
            specified"
            );
        }
        server_task.await?;
        return Ok(());
    }

    if args.elf_files_to_debug.is_empty()
        && !args.no_discover_elf
        && let Some(project) = &*project
    {
        debug!("The user didn't specify any files to debug, checking the project dir");
        args.elf_files_to_debug.extend(project.discover_elf_files());
    }

    let debugger = if let Some(client) = &args.client {
        Debugger::from_user(client)?
    } else {
        debug!("Searching for known debuggers");
        Debugger::find()?
    };
    debug!(
        debugger = ?debugger,
        elf_files = ?args.elf_files_to_debug,
        "Starting up debugger subprocess"
    );
    debugger
        .start(&args.elf_files_to_debug, &args.client_args)
        .await?;

    Ok(())
}

/// Sets up logging to stderr.
///
/// The `V5GDB_LOG` environment variable is used as a filter. The default log level is Warn.
fn init_logging(debug_io: bool) -> anyhow::Result<()> {
    let mut filter = EnvFilter::builder()
        .with_env_var("V5GDB_LOG")
        .with_default_directive(tracing::Level::WARN.into())
        .from_env()?;
    if debug_io {
        filter = filter.add_directive("v5gdb::serial=trace".parse()?);
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    Ok(())
}

async fn serve_device_serial(
    device: SerialDevice,
    server: TcpListener,
    user_io: UserIoMode,
) -> anyhow::Result<()> {
    let mut dev_conn = device.connect(Duration::from_secs(5))?;

    // Pause the program in case it's running so that GDB can attach to it.
    const CTRL_C: u8 = 0x03;
    debug!("Sending interrupt to device");
    dev_conn.write_user(&[CTRL_C]).await?;

    let mut program_output = [0; 2048];
    let mut program_input = [0; 4096];

    let error_handler: ErrorHandler = Box::new(move |err| match err {
        SerialStreamError::Serial(err) => error!("Device disconnected: {err}"),
        SerialStreamError::PacketParse { source, raw_frame } => {
            trace!(%source, raw_frame = ?String::from_utf8_lossy(&raw_frame), "unknown frame");
        }
        SerialStreamError::CobsDecode { source, raw_frame } => {
            trace!(%source, raw_frame = ?String::from_utf8_lossy(&raw_frame), "invalid COBS frame");
        }
    });

    let user_out = stderr();
    debug!(mode = ?user_io, "Setting up user I/O decoder");
    let user_out: Box<dyn AsyncWrite + Unpin + Send> = match user_io {
        UserIoMode::Auto => unreachable!(),
        UserIoMode::Pros => Box::new(ProsDecoder::new(user_out)),
        UserIoMode::Raw => Box::new(user_out),
    };

    let (client, _addr) = server.accept().await?;
    let mut stream = V5SerialStream::new(dev_conn, client, user_out, error_handler);

    loop {
        tokio::select! {
            read = stream.device.read_user(&mut program_output) => {
                match read {
                    Ok(0) => {
                        error!("Device disconnected");
                        break;
                    }
                    Ok(size) => stream.handle_device_data(&program_output[..size]).await?,
                    Err(err) => {
                        error!("Device disconnected: {err}");
                        break;
                    }
                }
            },
            read = stream.client.read(&mut program_input) => {
                match read {
                    Ok(0) => break,
                    Ok(size) => {
                        let good = stream.handle_client_data(&program_input[..size]).await;
                        if !good {
                            // The device was likely unplugged. Exit cleanly instead of
                            // panicking on the failed write.
                            break;
                        }
                    }
                    Err(err) => {
                        error!("GDB connection closed: {err}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
