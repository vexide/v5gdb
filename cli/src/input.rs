//! Forwarding of user input to the program running under v5gdb.

use std::{
    future::pending,
    io::{ErrorKind, Read, stdin},
    thread,
};

use tokio::sync::mpsc;
use tracing::{debug, error};

/// Stream of bytes received from the user which are intended to be sent to the running program.
pub struct UserInput(Option<mpsc::Receiver<Vec<u8>>>);

impl UserInput {
    /// Starts reading from stdin using a background thread.
    pub fn stdin() -> Self {
        // We intentionally don't use tokio::io::stdin here. Tokio docs: "For interactive uses, it
        // is recommended to spawn a thread dedicated to user input and use blocking IO directly in
        // that thread."
        let (tx, rx) = mpsc::channel(16);
        thread::spawn(move || {
            let mut stdin = stdin().lock();
            let mut buf = [0; 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(size) => {
                        if tx.blocking_send(buf[..size].to_vec()).is_err() {
                            // Stop if the UserInput got dropped.
                            break;
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    Err(err) => {
                        error!("failed to read stdin: {err}");
                        break;
                    }
                }
            }
        });

        Self(Some(rx))
    }

    /// Doesn't read any input, and [`Self::next`] never resolves.
    pub fn none() -> Self {
        Self(None)
    }

    /// Waits for the next chunk of input.
    ///
    /// Returns `None` once when the input ends, e.g. on EOF or error. After that, the returned
    /// future will never resolve. This function is cancel-safe.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        if let Some(rx) = &mut self.0 {
            if let Some(data) = rx.recv().await {
                return Some(data);
            }
            debug!("Reached the end of stdin");
            self.0 = None;
            return None;
        }
        pending().await
    }
}
