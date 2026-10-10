use core::{
    cell::UnsafeCell,
    error::Error,
    fmt::{self, Debug, Display},
    marker::PhantomData,
};

use gdbstub::conn::{Connection, ConnectionExt};
use vex_sdk::vexTasksRun;

use crate::sdk::serial::{self, Channel};

#[cfg(target_arch = "arm")]
pub mod mux;

#[derive(Debug)]
pub struct TransportError(pub &'static str);

impl Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for TransportError {}

impl From<&'static str> for TransportError {
    fn from(value: &'static str) -> Self {
        Self(value)
    }
}

/// Method of communication between debugger and host.
///
/// # Safety
///
/// Types implementing this trait must have an implementation of [`Transport::try_read`] which is
/// safe to call from an interrupt context. The default implementation of `try_read` calls
/// [`ConnectionExt::peek`] and [`ConnectionExt::read`].
pub unsafe trait Transport:
    ConnectionExt + Connection<Error = TransportError> + Send + 'static
{
    /// Attempts to read a byte from the transport.
    ///
    /// Returns the next byte if one is available, or else `None`. This function is safe to call
    /// from an interrupt context.
    fn try_read(&mut self) -> Result<Option<u8>, Self::Error> {
        if self.peek()?.is_some() {
            self.read().map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Debug logging via stdio.
///
/// When using this transport, input and output are muxed as [described in the
/// wiki](https://github.com/vexide/v5gdb/wiki/Transports#stdio-transport-usb-serial).
#[derive(Debug)]
#[non_exhaustive]
pub struct StdioTransport {
    // Serial reading is single-consumer.
    _unsync: PhantomData<UnsafeCell<()>>,
}

// SAFETY: Serial reading is safe to perform from an interrupt context because it just reads from
// a lock-free SPSC ringbuffer which is filled asynchronously by CPU0. To ensure there is actually
// only one consumer at a time we disable user reads via [`mux::enable_auto_muxing`] and unimplement
// Sync. See also <https://internals.vexide.dev/sdk/tasks#system-tasks>.
unsafe impl Transport for StdioTransport {
    fn try_read(&mut self) -> Result<Option<u8>, Self::Error> {
        // `vexSerialPeekChar` doesn't seem to work properly in ISRs, so we need an explicit
        // implementation here.
        Ok(serial::read_byte(Channel::USER))
    }
}

impl StdioTransport {
    pub const fn new() -> Self {
        Self {
            _unsync: PhantomData,
        }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Connection for StdioTransport {
    type Error = TransportError;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        #[cfg(target_arch = "arm")]
        mux::write_all(mux::ChannelId::Debug, &[byte]);
        Ok(())
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        #[cfg(target_arch = "arm")]
        mux::write_all(mux::ChannelId::Debug, buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        #[cfg(target_arch = "arm")]
        mux::flush_serial();
        Ok(())
    }

    fn on_session_start(&mut self) -> Result<(), Self::Error> {
        #[cfg(target_arch = "arm")]
        mux::enable_auto_muxing();

        Ok(())
    }
}

impl ConnectionExt for StdioTransport {
    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        Ok(serial::peek_byte(Channel::USER))
    }

    fn read(&mut self) -> Result<u8, Self::Error> {
        loop {
            if let Some(byte) = serial::read_byte(Channel::USER) {
                return Ok(byte);
            }

            unsafe {
                vexTasksRun();
            }
        }
    }
}
