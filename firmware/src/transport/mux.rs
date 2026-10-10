//! Automatic framing of user serial writes.
//!
//! This module contains alternative implementations of certain VEX SDK functions which capture
//! calls to functions like `vexSerial*` to automatically add framing via a multiplexing protocol.
//!
//! The current implementation is compatible with the following SDKs:
//!
//! - vex_sdk_jumptable
//! - v5rt (aka. VEXcode SDK)
//! - v5rts (aka. VEX Partner SDK)

#![allow(non_snake_case)]

use core::cmp;

use cobs::CobsEncoder;

use crate::sdk::serial::{self, Channel};

/// Capture calls to `vexSerial*` functions and automatically add multiplexing packet framing,
/// sending them over the User channel.
pub fn enable_auto_muxing() {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChannelId {
    /// Standard I/O.
    User = b'u',
    /// Debug channel. Messages should use the GDB Protocol.
    Debug = b'd',
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
const MAX_PAYLOAD_LEN: usize = PACKET_BUF_SIZE - 3;
#[expect(clippy::int_plus_one)]
const _: () = assert!(cobs::max_encoding_length(1 + MAX_PAYLOAD_LEN) + 1 <= PACKET_BUF_SIZE);

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

pub fn flush_serial() {
    unsafe {
        while serial::write_buf_capacity(serial::Channel::USER).unwrap() != serial::OUT_BUF_SIZE {
            vex_sdk::vexTasksRun();
        }
    }
}

/// User-visible serial interface.
///
/// This module contains replacements for the standard `vexSerial*` functions which automatically
/// add additional framing so that serial consumers can easily differentiate debug data from
/// standard user I/O. Currently only writes are muxed and reads are simply ignored.
mod user {
    use core::sync::atomic::{AtomicBool, Ordering};

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

    fn tried_to_read() {
        static PROGRAM_TRIED_TO_READ: AtomicBool = AtomicBool::new(false);
        if !PROGRAM_TRIED_TO_READ.swap(true, Ordering::Relaxed) {
            crate::logging::warn!(
                "Reading from serial while the debugger is active is unimplemented!"
            );
        }
    }

    pub extern "C" fn read_char(channel: u32) -> i32 {
        let channel = Channel::from(channel);
        if channel == Channel::USER {
            tried_to_read();
            return -1;
        }

        serial::read_byte(channel).map(|n| n as i32).unwrap_or(-1)
    }

    pub extern "C" fn peek_char(channel: u32) -> i32 {
        let channel = Channel::from(channel);
        if channel == Channel::USER {
            tried_to_read();
            return -1;
        }

        serial::peek_byte(channel).map(|n| n as i32).unwrap_or(-1)
    }
}
