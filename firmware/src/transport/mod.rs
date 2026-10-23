use core::{
    cell::UnsafeCell,
    cmp,
    error::Error,
    fmt::{self, Debug, Display},
    marker::PhantomData,
};

use vex_sdk::vexTasksRun;

use crate::sdk::serial::{self, Channel};

pub(crate) mod link;
#[cfg(target_arch = "arm")]
pub mod mux;

/// An error reported by a [`Transport`].
#[derive(Debug, Clone, Copy)]
pub struct TransportError(&'static str);

impl TransportError {
    /// Creates an error with the given message.
    #[must_use]
    pub const fn new(message: &'static str) -> Self {
        Self(message)
    }

    /// Returns the error's message.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.0
    }
}

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
/// [`Transport::try_read`] must be safe to call from an interrupt context. The default
/// implementation of that function calls [`Transport::peek`] and [`Transport::read`], so if it
/// isn't overridden, those must be interrupt-safe too.
pub unsafe trait Transport: Send + 'static {
    /// Writes a buffer of packet data to GDB.
    fn write_all(&mut self, buf: &[u8]) -> Result<(), TransportError>;

    /// Writes a single byte of packet data to GDB.
    fn write(&mut self, byte: u8) -> Result<(), TransportError> {
        self.write_all(&[byte])
    }

    /// Sends any buffered writes to GDB.
    fn flush(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    /// Returns the next byte received from GDB without consuming it, or `None` if no byte is
    /// available yet.
    fn peek(&mut self) -> Result<Option<u8>, TransportError>;

    /// Reads the next byte received from GDB, waiting until one is available.
    fn read(&mut self) -> Result<u8, TransportError>;

    /// Reads the next byte received from GDB, or returns `None` if no byte is available yet.
    ///
    /// This function must be safe to call from an interrupt context.
    fn try_read(&mut self) -> Result<Option<u8>, TransportError> {
        if self.peek()?.is_some() {
            self.read().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Called whenever a GDB session starts or restarts.
    fn on_session_start(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// Debug logging via stdio.
///
/// When using this transport, input and output are muxed as [described in the
/// wiki](https://github.com/vexide/v5gdb/wiki/Transports#stdio-transport-usb-serial).
#[derive(Debug)]
#[non_exhaustive]
pub struct StdioTransport {
    buffer: heapless::Vec<u8, { mux::MAX_PAYLOAD_LEN }>,
    // Serial reading is single-consumer.
    _unsync: PhantomData<UnsafeCell<()>>,
}

impl StdioTransport {
    pub const fn new() -> Self {
        Self {
            _unsync: PhantomData,
            buffer: heapless::Vec::new(),
        }
    }

    fn tx_packet(&mut self) {
        #[cfg(target_arch = "arm")]
        mux::write_all(mux::ChannelId::Debug, &self.buffer);
        self.buffer.clear();
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: Serial reading is safe to perform from an interrupt context because it just reads from
// a lock-free SPSC ringbuffer which is filled asynchronously by CPU0. To ensure there is actually
// only one consumer at a time we disable user reads via [`mux::enable_auto_muxing`] and unimplement
// Sync. See also <https://internals.vexide.dev/sdk/tasks#system-tasks>.
unsafe impl Transport for StdioTransport {
    fn write(&mut self, byte: u8) -> Result<(), TransportError> {
        if self.buffer.is_full() {
            self.tx_packet();
        }
        self.buffer.push(byte).unwrap();
        Ok(())
    }

    fn write_all(&mut self, mut buf: &[u8]) -> Result<(), TransportError> {
        while !buf.is_empty() {
            if self.buffer.is_full() {
                self.tx_packet();
            }

            let remaining_in_packet =
                cmp::min(buf.len(), self.buffer.capacity() - self.buffer.len());
            let (slice, rest) = buf.split_at(remaining_in_packet);

            buf = rest;
            if slice.len() == self.buffer.capacity() {
                #[cfg(target_arch = "arm")]
                mux::write_all(mux::ChannelId::Debug, slice);
            } else {
                self.buffer.extend(slice.iter().copied());
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), TransportError> {
        self.tx_packet();
        #[cfg(target_arch = "arm")]
        serial::flush();
        Ok(())
    }

    fn on_session_start(&mut self) -> Result<(), TransportError> {
        #[cfg(target_arch = "arm")]
        mux::enable_auto_muxing();

        Ok(())
    }

    fn peek(&mut self) -> Result<Option<u8>, TransportError> {
        Ok(serial::peek_byte(Channel::USER))
    }

    fn read(&mut self) -> Result<u8, TransportError> {
        loop {
            if let Some(byte) = serial::read_byte(Channel::USER) {
                return Ok(byte);
            }

            unsafe {
                vexTasksRun();
            }
        }
    }

    fn try_read(&mut self) -> Result<Option<u8>, TransportError> {
        // `vexSerialPeekChar` doesn't seem to work properly in ISRs, so we need an explicit
        // implementation here.
        Ok(serial::read_byte(Channel::USER))
    }
}
