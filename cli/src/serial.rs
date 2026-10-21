use std::{borrow::Cow, io, mem};

use cobs::{CobsDecoderOwned, DecodeError};
use thiserror::Error;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt, Stderr, stderr},
    net::TcpStream,
};
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

pub struct V5SerialStream<U> {
    pub device: SerialConnection,
    pub client: TcpStream,
    decoder: CobsDecoderOwned,
    /// The raw bytes of the frame currently being decoded, excluding delimiters.
    raw_frame: Vec<u8>,
    user_out: U,
    debug_out: Stderr,
    error_handler: ErrorHandler,
}

pub type ErrorHandler = Box<dyn FnMut(SerialStreamError<'_>) + Send>;

impl<U: AsyncWrite + Unpin> V5SerialStream<U> {
    pub fn new(
        device: SerialConnection,
        client: TcpStream,
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
    pub async fn handle_client_data(&mut self, mut buf: &[u8]) -> bool {
        trace!(body = %String::from_utf8_lossy(buf), "gdb -> dev[gdb]");

        while !buf.is_empty() {
            match self.device.write_user(buf).await {
                Ok(written) => {
                    buf = &buf[written..];
                }
                Err(err) => {
                    // The device was likely unplugged.
                    (self.error_handler)(SerialStreamError::Serial(err));
                    return false;
                }
            }
        }

        true
    }
}

#[derive(Debug, Error)]
#[error("unknown channel {:?} ({:#x?})", self.0 as char, self.0)]
pub struct UnknownChannel(u8);

#[derive(Debug)]
enum Channel {
    User,
    Debugger,
    DebuggerLog,
}

impl TryFrom<u8> for Channel {
    type Error = UnknownChannel;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            b'u' => Ok(Self::User),
            b'd' => Ok(Self::Debugger),
            b'l' => Ok(Self::DebuggerLog),
            other => Err(UnknownChannel(other)),
        }
    }
}

#[derive(Debug, Error)]
pub enum PacketParseError {
    #[error(transparent)]
    UnknownChannel(#[from] UnknownChannel),
    #[error("channel missing from packet")]
    MissingChannel,
}

struct Packet<'a> {
    channel: Channel,
    body: &'a [u8],
}

impl<'a> Packet<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, PacketParseError> {
        let Some(&channel_byte) = data.first() else {
            return Err(PacketParseError::MissingChannel);
        };

        let channel = Channel::try_from(channel_byte)?;
        let body = &data[1..];

        Ok(Self { channel, body })
    }
}
