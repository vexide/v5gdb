use std::{borrow::Cow, io};

use cobs::{CobsDecoderOwned, DecodeError, DecodeReport};
use thiserror::Error;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
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
    #[error("failed to decode COBS frame (invalid byte: {invalid_byte:#x?})")]
    CobsDecode {
        source: DecodeError,
        invalid_byte: u8,
    },
    #[error("failed to communicate with device")]
    Serial(#[from] SerialError),
}

pub struct V5SerialStream<U> {
    pub device: SerialConnection,
    pub client: TcpStream,
    decoder: CobsDecoderOwned,
    user_out: U,
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
            user_out,
            error_handler,
        }
    }

    /// Handle incoming raw data from remote device.
    ///
    /// This will try to coerce any incoming data into packets from v5gdb. Any invalid data is just
    /// printed to the user output stream.
    pub async fn handle_device_data(&mut self, mut incoming_bytes: &[u8]) -> io::Result<()> {
        while !incoming_bytes.is_empty() {
            // If we receive any invalid packets, they might be prints from the user, so
            // we should print them out as-is.

            match self.decoder.push(incoming_bytes) {
                Ok(Some(report)) => {
                    self.decoder.reset();
                    let (raw_frame, rest) = incoming_bytes.split_at(report.parsed_size());
                    self.handle_device_frame(report, raw_frame).await?;
                    incoming_bytes = rest;
                }
                Err(err) => {
                    self.decoder.reset();

                    // We are only discarding one byte, so print that one.
                    let invalid_byte = incoming_bytes[0];
                    self.user_out.write_all(&[invalid_byte]).await?;
                    (self.error_handler)(SerialStreamError::CobsDecode {
                        source: err,
                        invalid_byte,
                    });

                    // Skip one byte to try to resynchronize
                    incoming_bytes = &incoming_bytes[1..];
                }
                Ok(None) => {
                    // The frame is split across reads, wait for more data.
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handles a complete incoming COBS frame by attempting to parse it as a packet.
    ///
    /// If the frame cannot be parsed, its raw contents (before COBS decoding) are printed to
    /// the user output stream.
    async fn handle_device_frame(
        &mut self,
        report: DecodeReport,
        raw_frame: &[u8],
    ) -> io::Result<()> {
        let packet_bytes = &self.decoder.dest()[..report.frame_size()];

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
                trace!(body = %String::from_utf8_lossy(packet.body), "device -> user");
                self.user_out.write_all(packet.body).await?;
            }
            Channel::Debugger => {
                trace!(body = %String::from_utf8_lossy(packet.body), "device -> gdb");
                self.client.write_all(packet.body).await?;
            }
        }

        Ok(())
    }

    /// Forwards data from the client to the device and returns whether it was sent.
    pub async fn handle_client_data(&mut self, mut buf: &[u8]) -> bool {
        trace!(body = %String::from_utf8_lossy(buf), "gdb -> device");

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
}

impl TryFrom<u8> for Channel {
    type Error = UnknownChannel;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            b'u' => Ok(Self::User),
            b'd' => Ok(Self::Debugger),
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
