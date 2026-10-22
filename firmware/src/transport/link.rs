//! GDB Remote Serial Protocol link layer.
//!
//! The GDB protocol has a simple reliability layer: the receiver of a packet replies with `+` if
//! its checksum was valid and `-` if the packet should be sent again. gdbstub doesn't implement the
//! retransmission half of this, and treats a `-`, a bad checksum, or any stray byte between packets
//! as a fatal error. If the connection is lossy, that could cause a fatal error.
//!
//! [`Link`] is a [`Transport`] wrapper that handles these cases itself, so gdbstub only ever sees
//! well-formed packets:
//!
//! - When GDB sends `-`, the last packet is sent again.
//! - Incoming packets are buffered until their checksum is verified. Corrupted packets are answered
//!   with `-` and dropped.
//! - Stray bytes between packets are dropped.
//! - If GDB sends the same request again before acknowledging our reply, it didn't see our reply in
//!   time. The copy is acknowledged and dropped so the request isn't run (and answered) twice.

use core::hash::Hasher;

use gdbstub::conn::{Connection, ConnectionExt};
use hash32::{FnvHasher, Hasher as _};
use spin::{Mutex, MutexGuard};

use crate::{
    logging,
    transport::{Transport, TransportError},
};

/// Wraps a [`Transport`] to handle acknowledgements and retransmissions for gdbstub.
///
/// See the [module-level documentation](self) for details.
pub struct Link<S> {
    inner: S,

    rx_state: RxState,
    /// Holds an incoming packet.
    rx_buf: &'static mut [u8],
    /// Size in bytes of the packet stored in `rx_buf`.
    rx_len: usize,
    /// Number of bytes of the received packet that have already been passed to gdbstub.
    rx_processed: usize,
    /// Sum of all received packet body bytes.
    rx_checksum: u8,
    /// Identifies the most recent request that was passed to gdbstub, until GDB acknowledges our
    /// reply to it.
    ///
    /// This allows the Link to ignore packet retransmissions by GDB, which can happen if the GDB
    /// stub doesn't reply fast enough.
    last_request: Option<PacketId>,

    tx_state: TxState,
    /// Holds the most recent outgoing packet so it can be sent again.
    tx_buf: &'static mut [u8],
    tx_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RxState {
    /// Waiting for the start of a new packet.
    Ready,
    /// Receiving the packet body.
    Body,
    /// Receiving the first checksum digit.
    Checksum1,
    /// Receiving the second checksum digit.
    Checksum2 { first_digit: Option<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxState {
    /// Not currently sending any packet.
    Outside,
    /// Sending the packet body.
    Body,
    /// Sending the checksum, with the given number of digits left.
    Checksum(u8),
}

/// Records basic information about a packet so that they can be cheaply compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PacketId {
    len: usize,
    hash: u32,
}

impl PacketId {
    fn new(packet: &[u8]) -> Self {
        let mut hasher = FnvHasher::default();
        hasher.write(packet);
        Self {
            len: packet.len(),
            hash: hasher.finish32(),
        }
    }
}

/// Sent to/from GDB to acknowledge receipt of a packet.
#[repr(u8)]
enum Acknowledge {
    /// Understood.
    Ack = b'+',
    /// Please retransmit.
    Nack = b'-',
}

impl<S: Transport> Link<S> {
    /// Creates a new link.
    ///
    /// `rx_buf` should be at least as large as gdbstub's packet buffer. `tx_buf` should be large
    /// enough to fit gdbstub's largest response.
    pub fn new(inner: S, rx_buf: &'static mut [u8], tx_buf: &'static mut [u8]) -> Self {
        Self {
            inner,
            rx_state: RxState::Ready,
            rx_buf,
            rx_len: 0,
            rx_processed: 0,
            rx_checksum: 0,
            last_request: None,
            tx_state: TxState::Outside,
            tx_buf,
            tx_len: 0,
        }
    }

    /// Forgets about any packets in progress, e.g. because a new GDB session is starting.
    fn reset(&mut self) {
        self.rx_state = RxState::Ready;
        self.rx_len = 0;
        self.rx_processed = 0;
        self.rx_checksum = 0;
        self.last_request = None;
        self.tx_state = TxState::Outside;
        self.tx_len = 0;
    }

    /// Returns the next byte of a validated packet without consuming it, if there is one.
    fn peek_valid_byte(&self) -> Option<u8> {
        if self.rx_state == RxState::Ready && self.rx_processed < self.rx_len {
            Some(self.rx_buf[self.rx_processed])
        } else {
            None
        }
    }

    /// Returns the next byte of a validated packet, if there is one.
    fn next_valid_byte(&mut self) -> Option<u8> {
        let byte = self.peek_valid_byte()?;
        self.rx_processed += 1;
        Some(byte)
    }

    /// Processes one incoming byte.
    ///
    /// Returns a byte that should be passed straight to gdbstub, if any. Once a complete packet
    /// has been validated, its bytes can be read with [`Self::next_valid_byte`].
    fn receive(&mut self, byte: u8) -> Result<Option<u8>, TransportError> {
        match self.rx_state {
            RxState::Ready => match byte {
                b'$' => {
                    self.rx_len = 0;
                    self.rx_processed = 0;
                    self.rx_checksum = 0;
                    self.push_rx(byte);
                    self.rx_state = RxState::Body;
                }
                b'+' => {
                    self.last_request = None;
                    return Ok(Some(byte));
                }
                b'-' => self.retransmit()?,
                // Ctrl-C
                0x03 => return Ok(Some(byte)),
                _ => logging::trace!("Dropping stray byte: {byte:#04x}"),
            },
            RxState::Body => {
                self.push_rx(byte);
                self.rx_state = if byte == b'#' {
                    RxState::Checksum1
                } else {
                    self.rx_checksum = self.rx_checksum.wrapping_add(byte);
                    RxState::Body
                };
            }
            RxState::Checksum1 => {
                self.push_rx(byte);
                self.rx_state = RxState::Checksum2 {
                    first_digit: parse_hex(byte),
                };
            }
            RxState::Checksum2 { first_digit: high } => {
                self.push_rx(byte);
                self.rx_state = RxState::Ready;

                let checksum = high.zip(parse_hex(byte)).map(|(high, low)| high << 4 | low);
                self.finish_packet(checksum)?;
            }
        }

        Ok(None)
    }

    /// Appends a byte to the incoming packet.
    ///
    /// If the packet is too large, the byte is dropped and the incoming packet is invalidated.
    fn push_rx(&mut self, byte: u8) {
        if let Some(slot) = self.rx_buf.get_mut(self.rx_len) {
            *slot = byte;
        }
        self.rx_len += 1;
    }

    /// Returns whether the receive buffer has overflowed.
    const fn rx_overflow(&self) -> bool {
        self.rx_len > self.rx_buf.len()
    }

    /// Decides what to do with a complete incoming packet.
    fn finish_packet(&mut self, checksum: Option<u8>) -> Result<(), TransportError> {
        if self.rx_overflow() {
            logging::warn!(
                "Dropping a {} byte packet from client (too large)",
                self.rx_len
            );
            return self.discard_packet(Acknowledge::Nack);
        }

        if checksum != Some(self.rx_checksum) {
            logging::warn!("Received a corrupted packet from client, requesting a retransmit");
            return self.discard_packet(Acknowledge::Nack);
        }

        let id = PacketId::new(&self.rx_buf[..self.rx_len]);
        if self.last_request == Some(id) {
            // GDB thinks we didn't see its request and is sending it again. This happens when our
            // ACK to its first request didn't go through, so either we were too slow to reply or
            // the transport dropped our reply.
            logging::warn!("Client sent a duplicate packet, trying to recover");

            // We've already processed this packet, so just ACK the duplicate. If our original reply
            // was just slow, this lets us avoid preemptively sending a reply to the *next* packet,
            // and if it was lost entirely, this tells GDB it's allowed to ask for a retransmit.
            return self.discard_packet(Acknowledge::Ack);
        }

        // This is a new request, so GDB is done with our previous reply.
        self.last_request = Some(id);
        self.rx_processed = 0;
        Ok(())
    }

    /// Drops the incoming packet and responds to GDB with the given acknowledgement byte.
    fn discard_packet(&mut self, response: Acknowledge) -> Result<(), TransportError> {
        self.rx_len = 0;
        self.inner.write(response as u8)?;
        self.inner.flush()
    }

    /// Returns whether the transmit buffer has overflowed.
    const fn tx_overflow(&self) -> bool {
        self.tx_len > self.tx_buf.len()
    }

    /// Sends the most recent outgoing packet again.
    fn retransmit(&mut self) -> Result<(), TransportError> {
        let has_valid_packet = self.tx_len != 0 && !self.tx_overflow();
        if !has_valid_packet || self.tx_state != TxState::Outside {
            logging::warn!("Client requested a retransmit, but there isn't any packet to send");
            return Ok(());
        }

        logging::warn!("Client requested a packet retransmit");
        self.inner.write_all(&self.tx_buf[..self.tx_len])?;
        self.inner.flush()
    }

    /// Records an outgoing byte so the packet it belongs to can be sent again.
    fn record_tx(&mut self, byte: u8) {
        match self.tx_state {
            TxState::Outside => {
                // Anything outside of packets, such as acknowledgements, aren't recorded.
                if byte != b'$' {
                    return;
                }
                self.tx_len = 0;
                self.tx_state = TxState::Body;
            }
            TxState::Body => {
                if byte == b'#' {
                    self.tx_state = TxState::Checksum(2);
                }
            }
            TxState::Checksum(remaining) => {
                self.tx_state = if remaining == 1 {
                    TxState::Outside
                } else {
                    TxState::Checksum(remaining - 1)
                };
            }
        }

        if let Some(slot) = self.tx_buf.get_mut(self.tx_len) {
            *slot = byte;
            self.tx_len += 1;
        }
    }
}

/// Parses an ASCII hex digit.
fn parse_hex(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|digit| digit as u8)
}

impl<S: Transport> Connection for Link<S> {
    type Error = TransportError;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        self.record_tx(byte);
        self.inner.write(byte)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        for &byte in buf {
            self.record_tx(byte);
        }
        self.inner.write_all(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }

    fn on_session_start(&mut self) -> Result<(), Self::Error> {
        self.reset();
        self.inner.on_session_start()
    }
}

impl<S: Transport> ConnectionExt for Link<S> {
    fn read(&mut self) -> Result<u8, Self::Error> {
        loop {
            if let Some(byte) = self.next_valid_byte() {
                return Ok(byte);
            }

            let byte = self.inner.read()?;
            if let Some(byte) = self.receive(byte)? {
                return Ok(byte);
            }
        }
    }

    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        match self.peek_valid_byte() {
            Some(byte) => Ok(Some(byte)),
            None => self.inner.peek(),
        }
    }
}

// SAFETY: The inner `try_read` is interrupt-safe.
unsafe impl<S: Transport> Transport for Link<S> {
    /// Reads a byte without processing it.
    ///
    /// This is only used to watch for Ctrl-C while the program is running, when GDB shouldn't be
    /// sending any packets, so the link's packet handling is skipped. Unlike
    /// [`ConnectionExt::read`], this never writes to the transport, so it's safe to call from
    /// an interrupt context.
    fn try_read(&mut self) -> Result<Option<u8>, Self::Error> {
        if let Some(byte) = self.next_valid_byte() {
            return Ok(Some(byte));
        }
        self.inner.try_read()
    }
}

/// A handle to a [`Link`] that's stored somewhere else.
///
/// gdbstub takes ownership of its connection and drops it when it encounters a fatal error. Giving
/// it a handle instead lets the link survive, so a new gdbstub instance can be created with it.
pub(crate) struct LinkHandle<S: 'static>(&'static Mutex<Link<S>>);

impl<S> LinkHandle<S> {
    /// Creates a handle to the given link.
    pub(crate) fn new(link: &'static Mutex<Link<S>>) -> Self {
        Self(link)
    }

    fn link(&self) -> MutexGuard<'static, Link<S>> {
        self.0.try_lock().unwrap()
    }
}

impl<S: Transport> Connection for LinkHandle<S> {
    type Error = TransportError;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        self.link().write(byte)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        self.link().write_all(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.link().flush()
    }

    fn on_session_start(&mut self) -> Result<(), Self::Error> {
        self.link().on_session_start()
    }
}

impl<S: Transport> ConnectionExt for LinkHandle<S> {
    fn read(&mut self) -> Result<u8, Self::Error> {
        self.link().read()
    }

    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        self.link().peek()
    }
}

// SAFETY: Forwards to `Link::try_read`, which is interrupt-safe.
unsafe impl<S: Transport> Transport for LinkHandle<S> {
    fn try_read(&mut self) -> Result<Option<u8>, Self::Error> {
        match self.0.try_lock() {
            Some(mut link) => link.try_read(),
            None => Ok(None),
        }
    }
}
