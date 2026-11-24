//! Terminal mode (`--terminal`), which connects the program's I/O to the terminal without starting
//! a debugger.

use tokio::io::{Sink, stdout};
use tracing::error;
use vex_v5_serial::{Connection, serial::SerialDevice};

use crate::{
    UserIoMode, connect_device, input::UserInput, make_error_handler, make_user_out,
    serial::V5SerialStream,
};

/// Prints the program's output and v5gdb's logs, and sends stdin to the program, until the device
/// disconnects or stdin ends.
///
/// Debugger packets are discarded.
pub async fn run(device: SerialDevice, user_io: UserIoMode) -> anyhow::Result<()> {
    let dev_conn = connect_device(&device)?;
    let user_out = make_user_out(stdout(), user_io);
    let mut stream = V5SerialStream::new(dev_conn, None::<Sink>, user_out, make_error_handler());
    let mut input = UserInput::stdin();

    let mut program_output = [0; 2048];
    loop {
        tokio::select! {
            read = stream.device.read_user(&mut program_output) => match read {
                Ok(0) => {
                    error!("Device disconnected");
                    break;
                }
                Ok(size) => stream.handle_device_data(&program_output[..size]).await?,
                Err(err) => {
                    error!("Device disconnected: {err}");
                    break;
                }
            },
            data = input.next() => {
                // Quit when stdin ends (Ctrl-D).
                let Some(data) = data else { break };
                if !stream.handle_user_input(&data).await {
                    break;
                }
            }
        }
    }

    Ok(())
}
