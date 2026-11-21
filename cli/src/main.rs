use std::{
    cell::OnceCell, env::current_dir, ffi::OsString, future::pending, io, path::PathBuf,
    process::exit, time::Duration,
};

use anyhow::Context;
use clap::{Parser, builder::styling};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, stderr},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::EnvFilter;
use vex_v5_serial::{
    Connection,
    serial::{SerialConnection, SerialDevice, find_devices},
};

use crate::{
    debugger::{Debugger, DebuggerKind},
    framework::{Project, ProjectKind},
    input::UserInput,
    serial::{ErrorHandler, SerialStreamError, V5SerialStream, pros::ProsDecoder},
};

mod debugger;
mod framework;
mod input;
mod serial;
mod terminal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_logging(args.debug_io)?;
    args.warn_if_ignored_args();

    if let Err(err) = run(args).await {
        error!("{err:?}");
    }
    Ok(())
}

async fn run(args: Args) -> anyhow::Result<()> {
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

pub const STYLES: styling::Styles = styling::Styles::styled()
    .header(styling::AnsiColor::Green.on_default().bold())
    .usage(styling::AnsiColor::Green.on_default().bold())
    .literal(styling::AnsiColor::Blue.on_default().bold())
    .placeholder(styling::AnsiColor::Cyan.on_default());

/// Use GDB to debug a VEX V5 robot running the v5gdb debug server.
///
/// This command interrupts the program on connect, so it can attach to robots that are running or
/// already stopped at a breakpoint. Use the V5GDB_LOG environment variable to enable internal CLI
/// logging. On-device logging can be controlled using GDB monitor commands.
#[derive(Debug, clap::Parser)]
#[command(name = "v5gdb", version, author, styles(STYLES))]
struct Args {
    /// Instead of starting GDB as a subprocess, open a GDB remote server on the given address.
    ///
    /// In this mode, the program's output is printed to stderr and stdin is sent to the program.
    #[clap(long)]
    tcp: Option<String>,
    /// Access a Brain's serial I/O over stdin/stdout without starting a debugger.
    ///
    /// Unlike the debugger modes, this does not pause the program when it connects. Compared to
    /// framework tools (e.g. pros terminal), this will also decode v5gdb's serial packets,
    /// allowing for less garbled output.
    #[clap(
        short,
        long,
        conflicts_with_all = ["tcp", "elf_files_to_debug", "client", "lldb", "client_args"],
    )]
    terminal: bool,
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
        let device = self
            .device
            .take()
            .expect("tcp server should not have already started");
        // Open the device before listening so that errors (e.g. another program using the serial
        // port) are reported before anything tries to connect.
        let dev_conn = connect_device(&device)?;

        let addr = self.args.tcp.as_deref().unwrap_or("127.0.0.1:35537");
        let server = TcpListener::bind(addr).await?;

        if self.args.tcp.is_some() {
            // This allows VS Code and others to know when we're ready to connect.
            eprintln!("Listening on {}", server.local_addr()?);
        }

        let user_io = self.args.user_io;
        // When GDB runs as a subprocess, it needs to use stdin itself.
        let input = if self.args.tcp.is_some() {
            UserInput::stdin()
        } else {
            UserInput::none()
        };
        Ok(tokio::spawn(async move {
            if let Err(err) = serve_device_serial(dev_conn, server, user_io, input).await {
                error!("TCP Server: {err:?}");
            }
        }))
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        self.resolve_args();

        if self.args.terminal {
            let device = self
                .device
                .take()
                .expect("device should not have been used yet");
            return terminal::run(device, self.args.user_io).await;
        }

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

fn make_error_handler() -> ErrorHandler {
    Box::new(move |err| match err {
        SerialStreamError::Serial(err) => error!("Device disconnected: {err}"),
        SerialStreamError::PacketParse { source, raw_frame } => {
            trace!(%source, raw_frame = ?String::from_utf8_lossy(&raw_frame), "unknown frame");
        }
        SerialStreamError::CobsDecode { source, raw_frame } => {
            trace!(%source, raw_frame = ?String::from_utf8_lossy(&raw_frame), "invalid COBS frame");
        }
    })
}

/// Wraps `out` in a decoder for the given user I/O format.
fn make_user_out<W: AsyncWrite + Unpin + Send + 'static>(
    out: W,
    user_io: UserIoMode,
) -> Box<dyn AsyncWrite + Unpin + Send> {
    debug!(mode = ?user_io, "Setting up user I/O decoder");
    match user_io {
        UserIoMode::Auto => unreachable!(),
        UserIoMode::Pros => Box::new(ProsDecoder::new(out)),
        UserIoMode::Raw => Box::new(out),
    }
}

/// Opens a connection to the V5's serial port.
fn connect_device(device: &SerialDevice) -> anyhow::Result<SerialConnection> {
    device
        .connect(Duration::from_secs(5))
        .context("Failed to open the V5's serial port (another program may be using it)")
}

/// Bridges the device's serial connection to GDB clients, one at a time.
///
/// Program output and input are forwarded the whole time, including while no client is connected.
/// When a client connects, the program is interrupted so GDB can attach. After it disconnects, a
/// new client can connect. This returns when the device is unplugged.
async fn serve_device_serial(
    dev_conn: SerialConnection,
    server: TcpListener,
    user_io: UserIoMode,
    mut input: UserInput,
) -> anyhow::Result<()> {
    let mut program_output = [0; 2048];
    let mut program_input = [0; 4096];

    let user_out = make_user_out(stderr(), user_io);
    let mut stream = V5SerialStream::new(dev_conn, None, user_out, make_error_handler());

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
            accepted = server.accept(), if stream.client.is_none() => {
                let (client, addr) = accepted?;
                debug!(%addr, "GDB connected");

                // Pause the program in case it's running so that GDB can attach to it. This is
                // sent before any of the client's data, so the device sees it first.
                debug!("Sending interrupt to device");
                serial::Packet::send_interrupt(&mut stream.device).await?;
                stream.client = Some(client);
            },
            read = read_client(&mut stream.client, &mut program_input) => {
                match read {
                    Ok(0) => {
                        info!("GDB disconnected, waiting for a new connection");
                        stream.client = None;
                    }
                    Ok(size) => {
                        let good = stream.handle_client_data(&program_input[..size]).await;
                        if !good {
                            // The device was likely unplugged. Exit cleanly instead of
                            // panicking on the failed write.
                            break;
                        }
                    }
                    Err(err) => {
                        warn!("GDB connection closed: {err}");
                        info!("GDB disconnected, waiting for a new connection");
                        stream.client = None;
                    }
                }
            },
            // Once this hits EOF, `next` will just stop resolving. Unlike --terminal, we don't
            // exit, because a GDB might still want to talk to the device.
            Some(data) = input.next() => {
                if !stream.handle_user_input(&data).await {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Reads from the GDB client, or waits forever if none has connected yet.
async fn read_client(client: &mut Option<TcpStream>, buf: &mut [u8]) -> io::Result<usize> {
    match client {
        Some(client) => client.read(buf).await,
        None => pending().await,
    }
}
