use std::{env::current_dir, path::PathBuf, process::exit, time::Duration};

use clap::Parser;
use tokio::{
    io::{AsyncReadExt, stderr},
    net::TcpListener,
    time::sleep,
};
use vex_v5_serial::{
    Connection,
    serial::{SerialDevice, find_devices},
};

use crate::{
    debugger::Debugger,
    framework::Project,
    serial::{ErrorHandler, SerialStreamError, V5SerialStream},
};

mod debugger;
mod framework;
mod serial;

/// Use GDB to debug a VEX V5 robot running the v5gdb debug server.
///
/// This command can only connect to robots that are currently stopped at a breakpoint.
#[derive(Debug, clap::Parser)]
struct Args {
    /// Instead of starting GDB as a subprocess, open a GDB remote server on the given address.
    #[clap(long)]
    tcp: Option<String>,
    /// List of ELF files to debug with GDB (has no effect in TCP mode).
    elf_files_to_debug: Vec<PathBuf>,
    /// Prints verbose serial I/O logs and errors to stdout.
    #[clap(long)]
    debug_io: bool,
    /// Disables auto-discovery of ELF files when none are specified.
    #[clap(long, env = "V5GDB_NO_DISCOVER_ELF")]
    no_discover_elf: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = Args::parse();

    let mut all_devices = find_devices()?;
    if all_devices.is_empty() {
        eprintln!("No V5 device is connected");
        exit(1);
    }
    let device = all_devices.remove(0);

    let addr = args.tcp.as_deref().unwrap_or("127.0.0.1:35537");
    let server = TcpListener::bind(addr).await?;
    let server_task = tokio::spawn(async move {
        serve_device_serial(device, server, args.debug_io)
            .await
            .unwrap()
    });

    if args.tcp.is_some() {
        server_task.await?;
        return Ok(());
    }

    if args.elf_files_to_debug.is_empty()
        && !args.no_discover_elf
        && let Ok(Some(project)) = Project::discover(&current_dir()?)
    {
        args.elf_files_to_debug.extend(project.discover_elf_files());
    }

    let debugger = Debugger::find()?;
    debugger.start(&args.elf_files_to_debug).await?;

    Ok(())
}

async fn serve_device_serial(
    device: SerialDevice,
    server: TcpListener,
    debug_io: bool,
) -> anyhow::Result<()> {
    let dev_conn = device.connect(Duration::from_secs(5))?;

    let mut program_output = [0; 2048];
    let mut program_input = [0; 4096];

    let error_handler: ErrorHandler = Box::new(move |err| match err {
        SerialStreamError::Serial(err) => eprintln!("Device disconnected: {err}"),
        SerialStreamError::PacketParse { raw_frame, .. } if debug_io => {
            println!("unknown: {:?}", String::from_utf8_lossy(&raw_frame));
        }
        SerialStreamError::CobsDecode { invalid_byte, .. } if debug_io => {
            println!("invalid: {:?}", String::from_utf8_lossy(&[invalid_byte]));
        }
        _ => {}
    });

    let (client, _addr) = server.accept().await?;
    let mut stream = V5SerialStream::new(dev_conn, client, stderr(), error_handler);
    stream.debug_io = debug_io;

    loop {
        tokio::select! {
            read = stream.device.read_user(&mut program_output) => {
                if let Ok(size) = read {
                    stream.handle_device_data(&program_output[..size]).await?;
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
                    _ => {}
                }
            }
        }

        sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}
