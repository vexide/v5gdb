use std::{borrow::Cow, io, iter, mem};

use cobs::{CobsDecoderOwned, DecodeError};
use thiserror::Error;
use tokio::io::{AsyncWrite, AsyncWriteExt, Stderr, stderr};
use tracing::trace;
use vex_v5_serial::{
    Connection,
    serial::{SerialConnection, SerialError},
};

pub mod pros;

#[derive(Debug, Error)]
pub enum SerialStreamError<'a> {
    #[error("failed to parse packet")]
    PacketParse {
        source: PacketParseError,
        raw_frame: Cow<'a, [u8]>,
    },
    #[error("failed to decode COBS frame")]
    CobsDecode {
        source: DecodeError,
        raw_frame: Cow<'a, [u8]>,
    },
    #[error("failed to communicate with device")]
    Serial(#[from] SerialError),
}

/// Demultiplexes v5gdb's serial stream.
///
/// User output is written to `U`, debugger logs to stderr, and debugger packets to `C` (the GDB
/// client).
pub struct V5SerialStream<U, C> {
    pub device: SerialConnection,
    pub client: C,
    decoder: CobsDecoderOwned,
    /// The raw bytes of the frame currently being decoded, excluding delimiters.
    raw_frame: Vec<u8>,
    user_out: U,
    debug_out: Stderr,
    error_handler: ErrorHandler,
}

pub type ErrorHandler = Box<dyn FnMut(SerialStreamError<'_>) + Send>;

impl<U: AsyncWrite + Unpin, C: AsyncWrite + Unpin> V5SerialStream<U, C> {
    pub fn new(
        device: SerialConnection,
        client: C,
        user_out: U,
        error_handler: ErrorHandler,
    ) -> Self {
        let decoder = CobsDecoderOwned::new(2048);
        Self {
            device,
            client,
            decoder,
            raw_frame: Vec::new(),
            user_out,
            debug_out: stderr(),
            error_handler,
        }
    }

    /// Handle incoming raw data from remote device.
    ///
    /// This will try to coerce any incoming data into packets from v5gdb. Any data that can't be
    /// decoded (such as output written before the device enabled muxing) is printed to the user
    /// output stream as-is.
    pub async fn handle_device_data(&mut self, incoming_bytes: &[u8]) -> io::Result<()> {
        for &byte in incoming_bytes {
            // Zero bytes are frame delimiters, so they're never part of a frame's contents.
            if byte != 0 {
                self.raw_frame.push(byte);
            }

            match self.decoder.feed(byte) {
                Ok(None) => {}
                Ok(Some(frame_size)) => {
                    let raw_frame = mem::take(&mut self.raw_frame);
                    self.handle_device_frame(frame_size, &raw_frame).await?;
                }
                Err(err) => {
                    // The data since the last delimiter might be prints from the user, so print it
                    // out as-is and start a new frame.
                    self.decoder.reset();
                    let raw_frame = mem::take(&mut self.raw_frame);
                    self.user_out.write_all(&raw_frame).await?;
                    (self.error_handler)(SerialStreamError::CobsDecode {
                        source: err,
                        raw_frame: Cow::Owned(raw_frame),
                    });
                }
            }
        }

        // `user_out` may buffer decoded output so it has to be flushed to make the data appear now.
        self.user_out.flush().await?;
        self.debug_out.flush().await?;

        Ok(())
    }

    /// Handles a complete incoming COBS frame by attempting to parse it as a packet.
    ///
    /// If the frame cannot be parsed, its raw contents (before COBS decoding) are printed to
    /// the user output stream.
    async fn handle_device_frame(&mut self, frame_size: usize, raw_frame: &[u8]) -> io::Result<()> {
        let packet_bytes = &self.decoder.dest()[..frame_size];

        let packet = match Packet::parse(packet_bytes) {
            Ok(packet) => packet,
            Err(err) => {
                // The frame might be a print from the user, so fall back to printing it as-is.
                self.user_out.write_all(raw_frame).await?;
                (self.error_handler)(SerialStreamError::PacketParse {
                    source: err,
                    raw_frame: Cow::Borrowed(raw_frame),
                });

                return Ok(());
            }
        };

        match packet.channel {
            Channel::User => {
                trace!(body = %String::from_utf8_lossy(packet.body), "dev[usr] -> usr");
                self.user_out.write_all(packet.body).await?;
            }
            Channel::DebuggerLog => {
                trace!(body = %String::from_utf8_lossy(packet.body), "dev[gdb] -> usr");
                self.debug_out.write_all(packet.body).await?;
            }
            Channel::Debugger => {
                trace!(body = %String::from_utf8_lossy(packet.body), "dev[gdb] -> gdb");
                self.client.write_all(packet.body).await?;
            }
        }

        Ok(())
    }

    /// Forwards data from the client to the device and returns whether it was sent.
    pub async fn handle_client_data(&mut self, buf: &[u8]) -> bool {
        trace!(body = %String::from_utf8_lossy(buf), "gdb -> dev[gdb]");
        self.send(Channel::Debugger, buf).await
    }

    /// Forwards user input (the program's stdin) to the device and returns whether it was sent.
    pub async fn handle_user_input(&mut self, buf: &[u8]) -> bool {
        trace!(body = %String::from_utf8_lossy(buf), "usr -> dev[usr]");
        self.send(Channel::User, buf).await
    }

    async fn send(&mut self, channel: Channel, buf: &[u8]) -> bool {
        match Packet::write_all(&mut self.device, channel, buf).await {
            Ok(()) => true,
            Err(err) => {
                // The device was likely unplugged.
                (self.error_handler)(SerialStreamError::Serial(err));
                false
            }
        }
    }
}

pub struct Packet<'a> {
    channel: Channel,
    body: &'a [u8],
}

impl<'a> Packet<'a> {
    /// The largest payload sent to the device in one packet.
    ///
    /// This limit helps to keep packets from different channels interleaved with low latency. The
    /// device decodes packets as a stream, so there isn't as much a buffer size issue.
    const MAX_PAYLOAD_LEN: usize = 128;

    /// Creates a packet sending `data` on the given channel.
    const fn new(channel: Channel, body: &'a [u8]) -> Self {
        Self { channel, body }
    }

    /// Serializes the packet as bytes.
    ///
    /// The packet is built in `packet_buf` without COBS encoding, then appended to `dest_buf` with
    /// a trailing null byte.
    fn encode(&self, packet_buf: &mut Vec<u8>, dest_buf: &mut Vec<u8>) {
        packet_buf.clear();
        packet_buf.push(self.channel.id());
        packet_buf.extend_from_slice(self.body);

        let prev_size = dest_buf.len();
        dest_buf.extend(iter::repeat_n(
            0,
            cobs::max_encoding_length(packet_buf.len()),
        ));

        let written = cobs::encode(packet_buf, &mut dest_buf[prev_size..]);
        dest_buf.truncate(prev_size + written);
        dest_buf.push(0);
    }

    /// Encodes `data` as packets on the given channel.
    fn encode_all(channel: Channel, data: &[u8]) -> Vec<u8> {
        let mut encode_buf = Vec::new();
        let mut packet_buf = Vec::with_capacity(Self::MAX_PAYLOAD_LEN + 1);

        for chunk in data.chunks(Self::MAX_PAYLOAD_LEN) {
            Packet::new(channel, chunk).encode(&mut packet_buf, &mut encode_buf);
        }

        encode_buf
    }

    /// Sends `data` to the device as packets on the given channel.
    async fn write_all(
        device: &mut SerialConnection,
        channel: Channel,
        data: &[u8],
    ) -> Result<(), SerialError> {
        let encoded = Self::encode_all(channel, data);
        let mut buf = encoded.as_slice();
        while !buf.is_empty() {
            let written = device.write_user(buf).await?;
            buf = &buf[written..];
        }
        Ok(())
    }

    /// Parses a non-COBS-encoded packet.
    fn parse(data: &'a [u8]) -> Result<Self, PacketParseError> {
        let Some(&channel_byte) = data.first() else {
            return Err(PacketParseError::MissingChannel);
        };

        let channel = Channel::try_from(channel_byte)?;
        let body = &data[1..];

        Ok(Self { channel, body })
    }

    /// Sends a GDB interrupt (Ctrl-C) to the device.
    pub async fn send_interrupt(device: &mut SerialConnection) -> Result<(), SerialError> {
        const CTRL_C: u8 = 0x03;
        // This starts with a packet delimiter so the device's decoder discards anything left over
        // from an earlier connection.
        device.write_user(&[0]).await?;
        Packet::write_all(device, Channel::Debugger, &[CTRL_C]).await
    }
}

#[derive(Debug, Error)]
pub enum PacketParseError {
    #[error(transparent)]
    UnknownChannel(#[from] UnknownChannel),
    #[error("channel missing from packet")]
    MissingChannel,
}

#[derive(Debug, Error)]
#[error("unknown channel {:?} ({:#x?})", self.0 as char, self.0)]
pub struct UnknownChannel(u8);

#[derive(Debug, Clone, Copy)]
enum Channel {
    User,
    Debugger,
    DebuggerLog,
}

impl Channel {
    const fn id(self) -> u8 {
        match self {
            Self::User => b'u',
            Self::Debugger => b'd',
            Self::DebuggerLog => b'l',
        }
    }
}

impl TryFrom<u8> for Channel {
    type Error = UnknownChannel;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        [Self::User, Self::Debugger, Self::DebuggerLog]
            .into_iter()
            .find(|channel| channel.id() == value)
            .ok_or(UnknownChannel(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long input should be split into multiple packets, each with the channel ID.
    #[test]
    fn encodes_packets() {
        let long_data: Vec<u8> = (0..300).map(|n| n as u8).collect();
        let encoded = Packet::encode_all(Channel::User, &long_data);

        let mut decoded = Vec::new();
        for frame in encoded.split(|&byte| byte == 0).filter(|f| !f.is_empty()) {
            let packet = cobs::decode_vec(frame).unwrap();
            assert_eq!(packet[0], b'u');
            assert!(packet.len() <= Packet::MAX_PAYLOAD_LEN + 1);
            decoded.extend_from_slice(&packet[1..]);
        }
        assert_eq!(decoded, long_data);
    }
}
