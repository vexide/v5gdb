//! Terminal mode (`--terminal`), which shows program output without starting a debugger.

use std::time::Duration;

use tokio::io::{sink, stdout};
use tracing::error;
use vex_v5_serial::{Connection, serial::SerialDevice};

use crate::{UserIoMode, make_error_handler, make_user_out, serial::V5SerialStream};

/// Prints the program's output and v5gdb's logs until the device disconnects.
///
/// Debugger packets are discarded. Nothing is sent to the device.
pub async fn run(device: SerialDevice, user_io: UserIoMode) -> anyhow::Result<()> {
    let dev_conn = device.connect(Duration::from_secs(5))?;
    let user_out = make_user_out(stdout(), user_io);
    let mut stream = V5SerialStream::new(dev_conn, sink(), user_out, make_error_handler());

    let mut program_output = [0; 2048];
    loop {
        match stream.device.read_user(&mut program_output).await {
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
    }

    Ok(())
}
