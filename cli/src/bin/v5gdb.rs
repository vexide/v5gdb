use std::{process::exit, time::Duration};

use clap::Parser;
use cobs::CobsDecoderOwned;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, stderr},
    net::TcpListener,
    process::Command,
    time::sleep,
};
use vex_v5_serial::{
    Connection,
    serial::{self, SerialDevice},
};
use which::which;

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

    let mut all_devices = serial::find_devices()?;
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
    let mut resolved_gdb = None;
    for name in known_gdb_names {
        if let Ok(path) = which(name) {
            resolved_gdb = Some(path);
        }
    }

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
    gdb.wait().await?;

    Ok(())
}

async fn serve_device_serial(
    device: SerialDevice,
    server: TcpListener,
    debug_io: bool,
) -> anyhow::Result<()> {
    let mut connection = device.connect(Duration::from_secs(5))?;

    let mut program_output = [0; 2048];
    let mut program_input = [0; 4096];

    let mut decoder = CobsDecoderOwned::new(2048);
    let mut stderr = stderr();

    let (mut conn, _addr) = server.accept().await?;

    loop {
        tokio::select! {
            read = connection.read_user(&mut program_output) => {
                if let Ok(size) = read {
                    let mut incoming_bytes = &program_output[..size];

                    while !incoming_bytes.is_empty() {
                        // If we receive any invalid packets, they might be prints from the user, so
                        // we should print them out as-is.

                        match decoder.push(incoming_bytes) {
                            Ok(Some(report)) => {
                                let packet = &decoder.dest()[..report.frame_size()];
                                let was_valid = handle_packet(
                                    &mut conn,
                                    &mut stderr,
                                    packet,
                                    debug_io,
                                ).await;

                                if !was_valid {
                                    // If we receive an invalid packet, fall back to assuming it's
                                    // just raw data and print it out.
                                    let body = &incoming_bytes[..report.parsed_size()];

                                    if debug_io {
                                        println!("unknown: {:?}", String::from_utf8_lossy(body));
                                    }

                                    stderr.write_all(body).await?;
                                }

                                decoder.reset();
                                incoming_bytes = &incoming_bytes[report.parsed_size()..];
                            }
                            Err(_) => {
                                // We are only discarding one byte, so print that one.
                                let invalid_byte = incoming_bytes[0];
                                stderr.write_all(&[invalid_byte]).await?;

                                if debug_io {
                                    println!("invalid: {:?}", String::from_utf8_lossy(&[invalid_byte]));
                                }

                                // Skip one byte and try to resynchronize
                                incoming_bytes = &incoming_bytes[1..];
                                decoder.reset();
                            },
                            _ => {}
                        }
                    }
                }
            },
            read = conn.read(&mut program_input) => {
                match read {
                    Ok(0) => break,
                    Ok(size) => {
                        if debug_io {
                            println!("< {}", String::from_utf8_lossy(&program_input[..size]));
                        }

                        connection.write_user(&program_input[..size]).await.unwrap();
                    }
                    _ => {}
                }
            }
        }

        sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

/// Handles a packet from the V5 device and returns whether it was valid.
async fn handle_packet(
    debug_out: &mut (impl AsyncWrite + Unpin),
    user_out: &mut (impl AsyncWrite + Unpin),
    packet: &[u8],
    debug_io: bool,
) -> bool {
    let Some(channel_byte) = packet.first() else {
        // Missing channel
        return false;
    };

    let body = &packet[1..];

    match channel_byte {
        b'u' => {
            if debug_io {
                print!("{}", String::from_utf8_lossy(body));
            }
            _ = user_out.write_all(body).await;
        }
        b'd' => {
            if debug_io {
                println!("> {}", String::from_utf8_lossy(body));
            }
            _ = debug_out.write_all(body).await;
        }
        // Unknown channel
        _ => return false,
    }

    true
}
