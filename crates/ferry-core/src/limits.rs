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
//! Every constant here is enforced somewhere in this crate, save for
//! [`MAX_SERVING_PER_PEER`], which `ferry-runtime` enforces because it is
//! the crate that runs the server loop a paired connection is served from.
//! A limit that nothing checks belongs in the plan, not in a module named
//! `limits`.

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

/// How long dialling one address from a QR pairing offer may take before
/// `pair_ik` gives up on it and tries the next, in seconds.
///
/// A QR offer can carry more than one address (`docs/engine-contract.md`
/// item 12), tried in order. A plain `TcpStream::connect` has no timeout of
/// its own: dialling an address nothing answers, rather than one that
/// actively refuses the connection, can otherwise hold the OS's own connect
/// timeout, which is tens of seconds and platform dependent, before it gives
/// up. A handful of unreachable addresses ahead of the right one would then
/// cost minutes instead of seconds. Three seconds is well past what a
/// reachable device on the same network takes to answer, and short enough
/// that trying every address in a realistic offer still finishes quickly.
pub const QR_ADDRESS_CONNECT_TIMEOUT_SECS: u64 = 3;

/// The largest number of accepted connections that may be mid handshake at
/// once.
///
/// A handshake takes work before either side has proved anything, so it is a
/// place one peer could exhaust the other by opening many connections and
/// finishing none of them. `tcp` counts a connection as pending from the
/// moment it is accepted until its handshake ends, and refuses the next one
/// once this many are already pending.
pub const MAX_PENDING_HANDSHAKES: u32 = 8;

/// How long a newly accepted connection may go before its first byte
/// arrives, in seconds, before `tcp` drops it.
///
/// `docs/audits/fable-security.md`, findings 1 and 4: without this, a
/// connection that is accepted and then sends nothing held its pending slot
/// for the whole [`HANDSHAKE_TIMEOUT_SECS`]. `tcp` checks this first, in
/// `Pending::negotiate`, before the version exchange that
/// `HANDSHAKE_TIMEOUT_SECS` bounds even starts.
pub const FIRST_BYTE_TIMEOUT_SECS: u64 = 2;

/// The largest number of accepted connections from one source address that
/// may be mid handshake at once.
///
/// `docs/audits/fable-security.md`, findings 1 and 4. [`MAX_PENDING_HANDSHAKES`]
/// bounds every address together; without a bound per address too, one
/// address opening connections and sending nothing on each can hold every
/// pending slot by itself, and every other address's connection then queues
/// behind them. `tcp` counts this by the connecting `IpAddr`, alongside
/// [`MAX_PENDING_HANDSHAKES`], which still applies on top of this one.
pub const MAX_PENDING_HANDSHAKES_PER_ADDR: u32 = 2;

/// The largest number of serving connections one paired peer may hold open
/// at once.
///
/// `docs/audits/fable-security.md`, finding 5: a paired key has already
/// proven itself, unlike a pending handshake, so nothing capped how many
/// connections one paired device could open and never read on. Each held
/// one thread, plus a serving slot, forever, since `tcp` also used to clear
/// the write timeout once a handshake finished. `ferry_runtime::engine`'s
/// `register_serving` enforces this, refusing a peer's ninth serving
/// connection the same way `tcp::Listener` refuses a ninth pending one:
/// dropped at once, nothing reported.
pub const MAX_SERVING_PER_PEER: u32 = 8;

/// The largest number of reads one byte range may take.
///
/// A range is fetched in pieces of at most [`MAX_READ_LEN`], one mebibyte
/// each. A chunk is at most sixteen mebibytes, `ChunkSize`'s own maximum, so
/// a correct peer never needs more than sixteen reads to fill one. This cap
/// leaves four times that room, for a peer that answers in smaller pieces,
/// and stops a peer that answers a byte or two at a time from holding the
/// loop for as many reads as the range has bytes. When the cap is reached,
/// `read_range` in `ferry-core`'s `session` module returns what it has read
/// so far as a short result, for the caller to judge.
pub const MAX_READS_PER_CHUNK: u32 = 64;
