use std::{
    io::{self, ErrorKind},
    pin::Pin,
    task::{Context, Poll, ready},
};

use cobs::CobsDecoderOwned;
use pin_project::pin_project;
use tokio::io::AsyncWrite;

/// The largest PROS frame that can be decoded.
const MAX_FRAME_SIZE: usize = 0x4000;

/// PROS prefixes every COBS frame with a 4-byte stream ID.
const STREAM_ID_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamId {
    Stdout,
    Stderr,
    KernelDebug,
    Jinx,
}

impl StreamId {
    fn parse(id: &[u8]) -> Option<Self> {
        match id {
            b"sout" => Some(Self::Stdout),
            b"serr" => Some(Self::Stderr),
            b"kdbg" => Some(Self::KernelDebug),
            b"jinx" => Some(Self::Jinx),
            _ => None,
        }
    }
}

/// An [`AsyncWrite`] adapter that decodes PROS serial protocol and writes their contents (ignoring
/// their channel) to an inner writer.
///
/// PROS serial frames are COBS-encoded messages followed by a zero byte. The first four bytes of
/// every decoded message are the channel, which is usually used to direct output to stdout/stderr.
/// This decoder strips all framing (including channel info) and forwards the data. Invalid frames
/// or frames with unknown channels are forwarded without any decoding.
#[pin_project]
pub struct ProsDecoder<W> {
    cobs: CobsDecoderOwned,
    /// The raw (still encoded) bytes of the frame currently being decoded.
    raw_frame: Vec<u8>,
    /// Bytes waiting to be written to `out`.
    pending: Vec<u8>,
    /// How many bytes of `pending` have been written.
    pending_written: usize,
    #[pin]
    out: W,
}

impl<W: AsyncWrite> ProsDecoder<W> {
    pub fn new(out: W) -> Self {
        Self {
            cobs: CobsDecoderOwned::new(MAX_FRAME_SIZE),
            raw_frame: Vec::new(),
            pending: Vec::new(),
            pending_written: 0,
            out,
        }
    }

    /// Writes all pending bytes, if any, to the inner writer.
    fn poll_flush_pending(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut this = self.project();

        while *this.pending_written < this.pending.len() {
            let buf = &this.pending[*this.pending_written..];
            match ready!(this.out.as_mut().poll_write(cx, buf))? {
                0 => {
                    return Poll::Ready(Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "failed to write the decoded packet",
                    )));
                }
                written => *this.pending_written += written,
            }
        }

        this.pending.clear();
        *this.pending_written = 0;
        Poll::Ready(Ok(()))
    }

    /// Consumes bytes from `buf` until a frame ends, then queues the frame's output.
    ///
    /// Returns the number of bytes consumed, or `None` if no frame was finished.
    fn decode_frame(self: Pin<&mut Self>, buf: &[u8]) -> Option<usize> {
        let this = self.project();

        for (idx, &byte) in buf.iter().enumerate() {
            // Skip zero bytes between frames.
            if byte == 0 && this.raw_frame.is_empty() {
                continue;
            }
            this.raw_frame.push(byte);

            match this.cobs.feed(byte) {
                Ok(None) => continue, // Need more data
                Ok(Some(frame_size)) => {
                    let frame = &this.cobs.dest()[..frame_size];
                    let stream = frame.get(..STREAM_ID_LEN).and_then(StreamId::parse);

                    match stream {
                        // Nothing actually uses this channel AFAIK, but it seems to be intended for
                        // machine-readable data only.
                        Some(StreamId::Jinx) => {}
                        Some(_) => this.pending.extend_from_slice(&frame[STREAM_ID_LEN..]),
                        // If the frame starts with bogus data it's probably not actually from PROS,
                        // so just forward the raw data
                        None => this.pending.extend_from_slice(this.raw_frame),
                    }
                }
                Err(_) => {
                    // The data might be a raw (non-COBS-encoded) print, so forward it as-is.
                    this.pending.extend_from_slice(this.raw_frame);
                }
            }

            this.cobs.reset();
            this.raw_frame.clear();
            return Some(idx + 1);
        }

        None // Reached the end without finding a full message.
    }
}

impl<W: AsyncWrite> AsyncWrite for ProsDecoder<W> {
    /// Feeds bytes into the decoder.
    ///
    /// This returns early after each complete frame so that the frame's output can be written
    /// before more data is decoded.
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready!(self.as_mut().poll_flush_pending(cx))?;

        let Some(consumed) = self.as_mut().decode_frame(buf) else {
            return Poll::Ready(Ok(buf.len()));
        };

        // Start a write now we don't have to wait for the next write/flush.
        _ = self.poll_flush_pending(cx);
        Poll::Ready(Ok(consumed))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.as_mut().poll_flush_pending(cx))?;
        self.project().out.poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.as_mut().poll_flush_pending(cx))?;
        self.project().out.poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn encode(stream: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut frame = stream.to_vec();
        frame.extend_from_slice(data);
        let mut encoded = cobs::encode_vec(&frame);
        encoded.push(0);
        encoded
    }

    async fn decode(input: &[u8]) -> Vec<u8> {
        let mut decoder = ProsDecoder::new(Vec::new());
        decoder.write_all(input).await.unwrap();
        decoder.flush().await.unwrap();
        decoder.out
    }

    /// The decoder can parse messages and will discard JINX data.
    #[tokio::test]
    async fn decodes_frames() {
        let mut input = encode(b"sout", b"hello ");
        input.extend(encode(b"serr", b"world\0!"));
        input.extend(encode(b"jinx", br#"{ "foo": "bar" }"#));
        assert_eq!(decode(&input).await, b"hello world\0!");
    }

    /// The decoder can parse messages even if it's not given the entire message at once.
    #[tokio::test]
    async fn decodes_split_frames() {
        let input = encode(b"sout", b"hello world");
        let mut decoder = ProsDecoder::new(Vec::new());
        for chunk in input.chunks(3) {
            decoder.write_all(chunk).await.unwrap();
        }
        assert_eq!(decoder.out, b"hello world");
    }

    /// Data that doesn't match the PROS serial format is left unchanged.
    #[tokio::test]
    async fn invalid_frames_unchanged() {
        let input = encode(b"abcd", b"xyz");
        assert_eq!(decode(&input).await, input);
    }
}
