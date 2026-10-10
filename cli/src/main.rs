use std::{process::exit, time::Duration};

use clap::Parser;
use tokio::{
    io::{AsyncReadExt, stderr},
    net::TcpListener,
    process::Command,
    time::sleep,
};
use vex_v5_serial::{
    Connection,
    serial::{SerialDevice, find_devices},
};
use which::which;

use crate::serial::{ErrorHandler, SerialStreamError, V5SerialStream};

mod serial;

#[derive(Debug, clap::Parser)]
struct Args {
    #[clap(long)]
    tcp: Option<String>,
    elf_files_to_debug: Vec<String>,
    #[clap(long)]
    debug_io: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

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

    let known_gdb_names = ["arm-none-eabi-gdb", "gdb-multiarch", "gdb"];
    let resolved_gdb = known_gdb_names.into_iter().find_map(|n| which(n).ok());

    let Some(resolved_gdb) = resolved_gdb else {
        eprintln!("Error: One of the following GDB executables must be installed.");
        eprintln!("{:?}", known_gdb_names);
        exit(1);
    };

    let mut cmd = Command::new(resolved_gdb);

    let mut elves = args.elf_files_to_debug.iter();
    if let Some(main_elf_file) = elves.next() {
        cmd.arg(format!("--eval-command=file {main_elf_file}"));
    }
    for elf_file in elves {
        cmd.arg(format!("--eval-command=add-symbol-file {elf_file}"));
    }

    cmd.arg("--eval-command=target remote :35537");

    let mut gdb = cmd.spawn()?;

    cfg_select! {
        // On Unix, ignore SIGINT (Ctrl-C) since it's delivered to the whole foreground process
        // group including GDB, which has its own logic to handle signals.
        unix => {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigint = signal(SignalKind::interrupt())?;
            let mut sigterm = signal(SignalKind::terminate())?;

            loop {
                tokio::select! {
                    status = gdb.wait() => {
                        status?;
                        break;
                    }
                    _ = sigint.recv() => {}
                    _ = sigterm.recv() => {}
                }
            }
        }
        // It's similar on Windows but we receive a Ctrl-C event / a Ctrl-Break event instead.
        windows => {
            use tokio::signal::windows::{ctrl_break, ctrl_c};

            let mut ctrl_c = ctrl_c()?;
            let mut ctrl_break = ctrl_break()?;

            loop {
                tokio::select! {
                    status = gdb.wait() => {
                        status?;
                        break;
                    }
                    _ = ctrl_c.recv() => {}
                    _ = ctrl_break.recv() => {}
                }
            }
        }
        _ => {
            gdb.wait().await?;
        }
    }

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
