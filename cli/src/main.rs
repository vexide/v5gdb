use std::{
    cell::OnceCell, env::current_dir, ffi::OsString, path::PathBuf, process::exit, time::Duration,
};

use clap::Parser;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, stderr},
    net::TcpListener,
    task::JoinHandle,
};
use tracing::{debug, error, trace, warn};
use tracing_subscriber::EnvFilter;
use vex_v5_serial::{
    Connection,
    serial::{SerialDevice, find_devices},
};

use crate::{
    debugger::{Debugger, DebuggerKind},
    framework::{Project, ProjectKind},
    serial::{ErrorHandler, SerialStreamError, V5SerialStream, pros::ProsDecoder},
};

mod debugger;
mod framework;
mod serial;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_logging(args.debug_io)?;
    args.warn_if_ignored_args();

    debug!("Scanning for VEX V5 devices to connect to...");
    let mut all_devices = find_devices()?;
    if all_devices.is_empty() {
        error!("No V5 device is connected");
        exit(1);
    }
    let device = all_devices.remove(0);
    debug!(device = ?device, "We'll use this device");

    let mut app = App::new(args, device)?;
    app.run().await?;

    Ok(())
}

/// A format which can be used to decode user serial output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
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
    /// The debug client to use. It must accept GDB's command-line arguments (or LLDB's, if
    /// `--lldb` is specified). By default, v5gdb searches for a suitable GDB executable in various
    /// locations.
    #[clap(short, long, env = "V5GDB_CLIENT")]
    client: Option<PathBuf>,
    /// Use LLDB as the debug client instead of GDB. This is turned on automatically if the
    /// `--client` executable's name contains "lldb".
    #[clap(long)]
    lldb: bool,
    /// Extra arguments to pass to the debug client.
    #[clap(short = 'C', long = "client-arg")]
    client_args: Vec<OsString>,
}

impl Args {
    fn debugger_kind(&self) -> DebuggerKind {
        if self.lldb {
            DebuggerKind::Lldb
        } else if let Some(client) = &self.client {
            let kind = DebuggerKind::guess_from_path(client);
            debug!(kind = ?kind, "Inferred debugger kind from --client");
            kind
        } else {
            DebuggerKind::Gdb
        }
    }

    fn make_debugger(&self) -> anyhow::Result<Debugger> {
        let kind = self.debugger_kind();
        if let Some(client) = &self.client {
            Ok(Debugger::from_user(client, kind)?)
        } else {
            debug!(kind = ?kind, "Searching for known debuggers");
            Ok(Debugger::find(kind)?)
        }
    }

    fn warn_if_ignored_args(&self) {
        if self.tcp.is_some()
            && (self.client.is_some() || !self.client_args.is_empty() || self.lldb)
        {
            warn!(
                "--client/--client-arg/--lldb was specified, but these don't do anything when \
            --tcp is specified"
            );
        }
    }
}

struct App {
    args: Args,
    project: OnceCell<Option<Project>>,
    device: Option<SerialDevice>,
    dir: PathBuf,
}

impl App {
    fn new(args: Args, device: SerialDevice) -> anyhow::Result<Self> {
        Ok(Self {
            args,
            device: Some(device),
            project: OnceCell::new(),
            dir: current_dir()?,
        })
    }

    fn project(&self) -> Option<&Project> {
        self.project
            .get_or_init(|| {
                debug!("Looking for a project directory");
                Project::discover(&self.dir).ok().flatten()
            })
            .as_ref()
    }

    /// Resolves any arguments that are in states like "Auto" to their real values.
    fn resolve_args(&mut self) {
        if self.args.user_io == UserIoMode::Auto {
            if let Some(project) = self.project()
                && project.kind() == ProjectKind::Pros
            {
                self.args.user_io = UserIoMode::Pros;
            } else {
                self.args.user_io = UserIoMode::Raw;
            }
            debug!(resolved = ?self.args.user_io, "Resolved 'Auto' user I/O mode");
        }
    }

    fn elf_files_to_debug(&self) -> Vec<PathBuf> {
        let mut elves = self.args.elf_files_to_debug.clone();
        if elves.is_empty()
            && !self.args.no_discover_elf
            && let Some(project) = self.project()
        {
            debug!("The user didn't specify any files to debug, checking the project dir");
            elves.extend(project.discover_elf_files());
        }

        elves
    }

    async fn start_tcp(&mut self) -> anyhow::Result<JoinHandle<()>> {
        let addr = self.args.tcp.as_deref().unwrap_or("127.0.0.1:35537");
        let server = TcpListener::bind(addr).await?;

        let device = self
            .device
            .take()
            .expect("tcp server should not have already started");

        if self.args.tcp.is_some() {
            // This allows VS Code and others to know when we're ready to connect.
            eprintln!("Listening on {}", server.local_addr()?);
        }

        let user_io = self.args.user_io;
        Ok(tokio::spawn(async move {
            serve_device_serial(device, server, user_io).await.unwrap()
        }))
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        self.resolve_args();
        let server_task = self.start_tcp().await?;

        if self.args.tcp.is_some() {
            debug!("TCP mode is on: won't spawn a debugger subprocess");
            server_task.await?;
            return Ok(());
        }

        let debugger = self.args.make_debugger()?;
        let elf_files_to_debug = self.elf_files_to_debug();
        let extra_args = &self.args.client_args;

        debugger.start(&elf_files_to_debug, extra_args).await?;

        Ok(())
    }
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
