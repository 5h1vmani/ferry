//! Every limit the protocol enforces, in one place.
//!
//! These exist so that one peer cannot exhaust the other. Some of them are hit
//! during ordinary use, not only under attack. A camera folder holding twenty
//! thousand photos is normal, and it does not fit in one frame.
//!
//! Almost all of these are local policy, not wire format. Two devices need not
//! agree on them. If this side caps a read at one mebibyte and the other caps
//! at half that, both still work, because the smaller side simply refuses and
//! the caller asks for less.
//!
//! Only two are normative, meaning a second implementation must match them:
//! [`MAX_FRAME_PAYLOAD`], because a peer cannot send a frame the other refuses
//! to read, and [`MAX_NOISE_PLAINTEXT`], because the Noise specification fixes
//! it.
//!
//! Every constant here is enforced somewhere in this crate. A limit that
//! nothing checks belongs in the plan, not in a module named `limits`.

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

/// The largest number of entries one `list` response may carry.
///
/// A folder with more entries than this is read across several calls, using
/// the cursor the response returns.
pub const MAX_LIST_ENTRIES: u32 = 1024;

/// The longest a path may be, in bytes.
pub const MAX_PATH_LEN: usize = 1024;

/// The largest plaintext a single Noise transport message may carry.
///
/// The Noise specification caps a transport message at 65535 bytes. The
/// authentication tag takes 16 of them.
pub const MAX_NOISE_PLAINTEXT: usize = 65535 - 16;

/// The largest number of chunks one manifest may hold.
///
/// A manifest is exchanged in a single frame during resume, and each chunk
/// costs 32 bytes. This cap keeps it inside [`MAX_FRAME_PAYLOAD`].
///
/// It also sets a ceiling on file size for a given chunk size. At one mebibyte
/// per chunk the ceiling is 32 gibibytes. A larger file needs a larger chunk
/// size, up to the 16 mebibyte maximum, which reaches 512 gibibytes.
pub const MAX_MANIFEST_CHUNKS: u32 = 32 * 1024;

/// The largest a stored manifest may be, in bytes.
///
/// A manifest holds a length, a chunk size, a count, one 32 byte value per
/// chunk, and a 32 byte root hash.
pub const MAX_MANIFEST_BYTES: usize = 8 + 4 + 4 + (MAX_MANIFEST_CHUNKS as usize * 32) + 32;

/// How long a handshake may run before the connection is dropped, in seconds.
///
/// A handshake is the version exchange plus the Noise messages that follow.
/// `tcp` sets this as the socket read and write timeout until the handshake
/// finishes, then clears it. This stops a client that connects and then
/// sends nothing from holding a slot forever.
pub const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// The largest number of accepted connections that may be mid handshake at
/// once.
///
/// A handshake takes work before either side has proved anything, so it is a
/// place one peer could exhaust the other by opening many connections and
/// finishing none of them. `tcp` counts a connection as pending from the
/// moment it is accepted until its handshake ends, and refuses the next one
/// once this many are already pending.
pub const MAX_PENDING_HANDSHAKES: u32 = 8;
