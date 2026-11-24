//! Multiplexing of user serial I/O and debugger traffic.
//!
//! This module contains alternative implementations of certain VEX SDK functions which capture
//! calls to functions like `vexSerial*` to automatically add framing via a multiplexing protocol.
//!
//! Both directions use the same format: each packet is COBS-encoded, starts with a [`ChannelId`]
//! byte, and ends with a `0` delimiter. Incoming packets are decoded by a [`Demux`] state machine.
//!
//! The current implementation is compatible with the following SDKs:
//!
//! - vex_sdk_jumptable
//! - v5rt (aka. VEXcode SDK)
//! - v5rts (aka. VEX Partner SDK)

#![allow(non_snake_case)]

use core::{
    cmp,
    sync::atomic::{AtomicBool, Ordering},
};

use cobs::{CobsEncoder, DecodeResult, DecoderState};
use heapless::spsc::{Consumer, Producer, Queue};
use spin::{Mutex, Once};
use static_cell::ConstStaticCell;

use crate::sdk::serial::{self, Channel};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// This is used to detect whether installing the mux was actually successful.
static MUXED_PEEK_CHAR_CALLED: AtomicBool = AtomicBool::new(false);

/// The number of bytes of user input that can be queued before the program reads them.
const USER_RX_CAPACITY: usize = 1024;

/// User input received from the host, waiting for the program to read it.
static USER_RX: UserRx = UserRx::new();

/// A queue of user input, filled by [`Demux`] and read by the program's `vexSerialReadChar` and
/// `vexSerialPeekChar` calls.
///
/// The producer is only used by the transport, which only runs while the debugger's session is
/// locked, so it's never contended. The consumer is used by the program's serial reads, which may
/// come from several tasks; a read that loses the race for the lock just sees no data.
///
/// None of these methods block, so they're safe to call from an interrupt context and from tasks
/// with small stacks.
struct UserRx {
    storage: ConstStaticCell<Queue<u8, USER_RX_CAPACITY>>,
    /// The two ends of the queue, created from `storage` on first use.
    ends: Once<QueueEnds>,
    /// Whether input has been dropped because the queue was full.
    overflowed: AtomicBool,
}

struct QueueEnds {
    producer: Mutex<Producer<'static, u8>>,
    consumer: Mutex<Consumer<'static, u8>>,
}

impl UserRx {
    const fn new() -> Self {
        Self {
            storage: ConstStaticCell::new(Queue::new()),
            ends: Once::new(),
            overflowed: AtomicBool::new(false),
        }
    }

    /// Returns the queue's ends, creating them if needed.
    ///
    /// Call this outside of interrupts before the queue is used from one, because an interrupt
    /// that arrives while another context is creating the ends would spin forever.
    fn ends(&'static self) -> &'static QueueEnds {
        self.ends.call_once(|| {
            let (producer, consumer) = self.storage.take().split();
            QueueEnds {
                producer: Mutex::new(producer),
                consumer: Mutex::new(consumer),
            }
        })
    }

    /// Queues a byte, or records that it was dropped if the queue is full.
    fn push(&'static self, byte: u8) {
        // The try_lock here could give a false positive overflow if we accidentally call this from
        // the wrong context, but it seems better than panicking.
        let queued = self
            .ends()
            .producer
            .try_lock()
            .is_some_and(|mut producer| producer.enqueue(byte).is_ok());
        if !queued {
            self.overflowed.store(true, Ordering::Relaxed);
        }
    }

    /// Removes and returns the next byte, if there is one.
    fn pop(&'static self) -> Option<u8> {
        self.ends()
            .consumer
            .try_lock()
            .and_then(|mut consumer| consumer.dequeue())
    }

    /// Returns the number of queued bytes.
    fn len(&'static self) -> usize {
        self.ends()
            .consumer
            .try_lock()
            .map_or(0, |consumer| consumer.len())
    }

    /// Returns whether input was dropped since the last call.
    fn take_overflowed(&self) -> bool {
        self.overflowed.swap(false, Ordering::Relaxed)
    }
}

/// Checks whether the serial mux has been enabled.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Logs a warning if user input has been dropped since the last call because the program didn't
/// read it fast enough.
pub fn warn_if_user_input_dropped() {
    if USER_RX.take_overflowed() {
        crate::logging::warn!(
            "Some serial input was dropped because the program didn't read it fast enough"
        );
    }
}

/// Capture calls to `vexSerial*` functions and automatically add multiplexing packet framing,
/// sending them over the User channel.
pub fn enable_auto_muxing() {
    ENABLED.store(true, Ordering::Relaxed);
    // Set up the input queue now, before the debugger's interrupt handler can use it.
    _ = USER_RX.ends();

    // Anything written before this point (e.g. vexide's startup banner) isn't framed and could
    // confuse the COBS decoder on the host, so reset its state by sending a packet delimiter.
    serial::write_all(serial::Channel::USER, &[0]).unwrap();

    unsafe {
        crate::sdk::redirect_function(
            vex_sdk::vexSerialWriteBuffer as *mut (),
            user::write_buffer as *const (),
        );

        crate::sdk::redirect_function(
            vex_sdk::vexSerialWriteChar as *mut (),
            user::write_char as *const (),
        );

        crate::sdk::redirect_function(
            vex_sdk::vexSerialReadChar as *mut (),
            user::read_char as *const (),
        );

        crate::sdk::redirect_function(
            vex_sdk::vexSerialPeekChar as *mut (),
            user::peek_char as *const (),
        );
    }

    assert!(
        check_redirects_active(),
        "v5gdb's StdioTransport could not be installed because some VEX SDK functions were inlined. \
         (Please disable LTO if enabled)"
    );
}

/// Checks whether calls to the `vexSerial*` SDK wrappers run the redirected proxies in [`user`].
fn check_redirects_active() -> bool {
    MUXED_PEEK_CHAR_CALLED.store(false, Ordering::Relaxed);
    unsafe { vex_sdk::vexSerialPeekChar(u32::MAX) };
    MUXED_PEEK_CHAR_CALLED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChannelId {
    /// Standard I/O.
    User = b'u',
    /// Debugger channel. Messages should use the GDB Protocol.
    Debug = b'd',
    /// Debugger logs channel.
    ///
    /// Data sent over this channel is never double-encoded, whereas User data may be encoded in the
    /// PROS serial wire format, for instance.
    DebugLog = b'l',
}

/// The size of the buffer used to build a single outgoing packet.
///
/// This is intentionally a lot smaller than the system's out buffer size to support tasks with very
/// small stack sizes (including the PROS serial daemon which only has 2 KiB of stack). We also
/// can't put this in a static because that would be corrupted if we paused during a [`write_all`].
const PACKET_BUF_SIZE: usize = 128;

/// The largest number of payload bytes that fit in one packet.
///
/// There are three bytes of overhead: the channel ID, COBS overhead, and finally a \0 delimiter.
pub const MAX_PAYLOAD_LEN: usize = PACKET_BUF_SIZE - 3;
#[expect(clippy::int_plus_one)]
const _: () = assert!(cobs::max_encoding_length(1 + MAX_PAYLOAD_LEN) + 1 <= PACKET_BUF_SIZE);

/// Decoder for the multiplexed serial stream sent by the host.
///
/// Bytes are decoded one at a time as they arrive, so no buffer is needed for whole packets.
/// Debugger bytes are returned to the transport and user bytes are queued for the program's
/// `vexSerialReadChar` and `vexSerialPeekChar` calls.
#[derive(Debug)]
pub struct Demux {
    cobs: DecoderState,
    channel: ChannelState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelState {
    /// The next decoded byte is the packet's channel ID.
    Pending,
    Known(ChannelId),
    /// The packet is malformed or has an unknown channel, so its contents are dropped.
    Invalid,
}

impl Demux {
    pub const fn new() -> Self {
        Self {
            cobs: DecoderState::Idle,
            channel: ChannelState::Pending,
        }
    }

    /// Processes one byte received from the host.
    ///
    /// Returns the decoded byte if it belongs to the debugger channel. Bytes on the user channel
    /// are queued for the program to read. This is safe to call from an interrupt context.
    pub fn feed(&mut self, byte: u8) -> Option<u8> {
        match self.cobs.feed(byte) {
            Ok(DecodeResult::NoData) => {}
            Ok(DecodeResult::DataStart | DecodeResult::DataComplete) | Err(_) => {
                self.channel = ChannelState::Pending;
            }
            Ok(DecodeResult::DataContinue(data)) => match self.channel {
                ChannelState::Pending => {
                    self.channel = match data {
                        b'u' => ChannelState::Known(ChannelId::User),
                        b'd' => ChannelState::Known(ChannelId::Debug),
                        // The Debugger Logs channel only makes sense in the device->host direction.
                        _ => ChannelState::Invalid,
                    };
                }
                ChannelState::Known(ChannelId::Debug) => return Some(data),
                ChannelState::Known(ChannelId::User) => USER_RX.push(data),
                ChannelState::Known(ChannelId::DebugLog) | ChannelState::Invalid => {}
            },
        }

        None
    }
}

impl Default for Demux {
    fn default() -> Self {
        Self::new()
    }
}

/// Write one or more COBS-encoded packets to serial output, each prefixed with the given channel
/// id.
///
/// Returns the number of bytes that were written from `buf`.
pub fn write_all(channel: ChannelId, mut buf: &[u8]) {
    while !buf.is_empty() {
        let mut out_buf = [0u8; PACKET_BUF_SIZE];

        // The actual out-buffer has 1 extra byte for the packet delimiter.
        let max_len = out_buf.len() - 1;
        let out_buf_without_delimiter = &mut out_buf[..max_len];

        let mut encoder = CobsEncoder::new(out_buf_without_delimiter);

        // Put as many bytes as possible into this packet. The chunk size has to be worked out in
        // advance because a failed `push` still advances the encoder's state (this is probably
        // a bug in the cobs crate).
        let (chunk, rest) = buf.split_at(cmp::min(buf.len(), MAX_PAYLOAD_LEN));
        buf = rest;

        encoder.push(&[channel as u8]).unwrap();
        encoder.push(chunk).unwrap();

        let length = encoder.finalize();

        // When the payload is exactly 254 bytes long, the COBS encoder will start a new 0-sized
        // block at index `length`, which means the pre-zeroed byte from when we first created the
        // buffer might not still be there. Thus we have to explicitly add it back in.
        out_buf[length] = 0;

        // Include `0` packet delimiter.
        serial::write_all(serial::Channel::USER, &out_buf[..=length]).unwrap();
    }
}

/// User-visible serial interface.
///
/// This module contains replacements for the standard `vexSerial*` functions which automatically
/// add additional framing so that serial consumers can easily differentiate debug data from
/// standard user I/O. Reads return the user input that [`Demux`] has received from the host.
mod user {
    use core::sync::atomic::Ordering;

    use super::*;

    pub extern "C" fn write_char(channel: u32, c: u8) -> i32 {
        let channel = Channel::from(channel);
        if channel == Channel::USER {
            write_all(ChannelId::User, &[c]);
            return 1;
        }

        serial::write_byte(channel, c) as i32
    }

    pub unsafe extern "C" fn write_buffer(channel: u32, data: *const u8, data_len: u32) -> i32 {
        let channel = Channel::from(channel);
        if channel == Channel::USER {
            let user_data = unsafe { core::slice::from_raw_parts(data, data_len as usize) };
            write_all(ChannelId::User, user_data);
            return data_len as i32;
        }

        unsafe {
            serial::write_buf(channel, data, data_len as usize)
                .map(|n| n as i32)
                .unwrap_or(-1)
        }
    }

    pub extern "C" fn read_char(channel: u32) -> i32 {
        let channel = Channel::from(channel);
        if channel == Channel::USER {
            return USER_RX.pop().map_or(-1, i32::from);
        }

        serial::read_byte(channel).map(|n| n as i32).unwrap_or(-1)
    }

    /// Returns the number of unread bytes (see [`serial::incoming_len`]). This does NOT "peek" the
    /// next character in the traditional sense! Matches behavior of `vexSerialPeekChar`.
    pub extern "C" fn peek_char(channel: u32) -> i32 {
        MUXED_PEEK_CHAR_CALLED.store(true, Ordering::Relaxed);

        let channel = Channel::from(channel);
        if channel == Channel::USER {
            return USER_RX.len() as i32;
        }

        serial::incoming_len(channel) as i32
    }
}
