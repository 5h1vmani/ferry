//! Every limit the protocol enforces, in one place.
//!
//! These exist so that one peer cannot exhaust the other. Some of them are hit
//! during ordinary use, not only under attack. A camera folder holding twenty
//! thousand photos is normal, and it does not fit in one frame.
//!
//! This module mirrors the limits table in `docs/protocol.md`. If a value
//! changes here, change it there too.

/// The largest frame payload, in bytes.
///
/// A `read` response carries up to [`MAX_READ_LEN`] bytes of file data plus a
/// small header. This leaves room for the header.
pub const MAX_FRAME_PAYLOAD: u32 = MAX_READ_LEN + 64 * 1024;

/// The largest number of bytes one `read` may ask for.
///
/// This bounds the buffer a server must hold for a single request. A chunk
/// larger than this is fetched with several reads, because reads carry a byte
/// range.
pub const MAX_READ_LEN: u32 = 1024 * 1024;

/// The largest number of bytes one `write` may carry.
pub const MAX_WRITE_LEN: u32 = MAX_READ_LEN;

/// The largest number of requests one connection may have in flight.
///
/// Pipelining without this cap is a memory attack. It is also how an ordinary
/// client accidentally asks for more than a phone can hold.
pub const MAX_REQUESTS_IN_FLIGHT: u32 = 64;

/// The largest number of response bytes a connection may owe at one moment.
///
/// Sixty four requests of one mebibyte each would otherwise ask a phone to
/// buffer sixty four mebibytes.
pub const MAX_OUTSTANDING_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// The largest number of entries one `list` response may carry.
///
/// A folder with more entries than this is read across several calls, using
/// the cursor the response returns.
pub const MAX_LIST_ENTRIES: u32 = 1024;

/// The longest a path may be, in bytes.
pub const MAX_PATH_LEN: usize = 1024;

/// How long a handshake may take before the connection is dropped.
///
/// Half-open handshakes must not accumulate. Any host on the network can start
/// one.
pub const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// How many connections may be waiting on a handshake at one moment.
pub const MAX_PENDING_HANDSHAKES: u32 = 8;

/// The largest plaintext a single Noise transport message may carry.
///
/// The Noise specification caps a transport message at 65535 bytes. The
/// authentication tag takes 16 of them.
pub const MAX_NOISE_PLAINTEXT: usize = 65535 - 16;
