//! The access log: what a paired device actually read or wrote.
//!
//! Job 9 in `docs/jobs.md`, item 13 in `docs/engine-contract.md`. This module
//! is a standalone store and roll-up. It is not wired into the engine yet:
//! nothing calls it, and the `uniffi` records the app will eventually see are
//! built later, on the boundary, from the plain types here.
//!
//! # Framing
//!
//! Each day's file starts with one version byte, written only the first
//! time that day is touched. After that come zero or more entries, each one
//! written as a length-prefixed byte string: a four byte big endian length,
//! then that many bytes of encoded fields. Reading stops, without failing
//! the whole file, at the first entry whose declared length runs past the
//! end of what is on disk. A day file is only ever appended to, so that can
//! only be the last entry, and it can only mean the process stopped before
//! finishing that one write. Every entry before it is still whole and is
//! still returned.
//!
//! # Storage
//!
//! One file per UTC day, at `<data_dir>/access_log/<YYYYMMDD>`, written by
//! opening in append mode and writing one framed entry. The whole file is
//! never rewritten to add an entry, unlike `record.rs` and `ferry-core`'s
//! `peers.rs`, which hold one value that changes as a whole. A day holds at
//! most 10,000 entries; [`AccessLog::append`] returns `Ok(false)` and writes
//! nothing once a day is full. An entry's id is `"<day>-<sequence>"`, where
//! `<sequence>` counts from 1 within the day and is never stored: it is
//! always just the entry's position among the whole entries in its day
//! file, so it is stable across restarts for free.
//!
//! # Rolling up
//!
//! A Finder browse or a file picker open is several file operations in a
//! row, not one. [`RollUp`] merges operations on the same connection, verb,
//! and path into one pending entry, adds their byte and item counts
//! together, and only writes it to the store once it is final: the
//! connection moves to a different path, five seconds pass with nothing new
//! on it, or the connection ends. `set_mtime` is never logged, because it
//! always follows a write that already is.
//!
//! A recursive walk pages one folder at a time, but it does not have to
//! finish paging a folder before it descends into a subfolder it already
//! saw a page of: on the serving side, that shows up as the same connection
//! listing `"Camera"`, then `"Camera/Sub"`, then `"Camera"` again to finish
//! it. Touching `"Camera/Sub"` finalises the pending `"Camera"` entry, since
//! a connection only has one path open at a time; touching `"Camera"` again
//! afterwards starts a fresh pending entry rather than reopening the one
//! already written out. So the parent folder's `List` ends up as more than
//! one entry on the serving side, one per unbroken run of pages on it,
//! rather than the single entry its own walk might suggest. This is
//! accepted rather than fixed: the alternative is keeping a pending entry
//! alive per path a connection has ever paused on, which is only bounded by
//! how deep and how interleaved a walk chooses to be, where finalising on
//! every path change keeps the pending table at at most a few entries no
//! matter how a walk is shaped.
//!
//! # Where it is wired in
//!
//! `engine.rs` opens the store at `start` from `data_dir`, holds the
//! [`RollUp`] behind a mutex, and, at `stop`, once every thread `stop` can
//! join has finished, takes it out, calls [`RollUp::finalize_all`] on it,
//! and drops it. That covers whatever a serving thread left pending too: a
//! serving thread is never joined (`lib.rs`, "a serving thread cannot be
//! woken"), so `finalize_all` is what keeps its last, still-pending entry
//! from being lost rather than a join ever waiting for it. `guard.rs` calls
//! [`RollUp::touch`] as actor [`Actor::Peer`] for a served connection, and
//! `engine.rs` and `transfer.rs` call it as actor [`Actor::This`] for `list`,
//! a transfer attempt's reads, and `pull_folder`. The boundary's
//! `AccessEntry`, `AccessVerb`, and `Actor` in `lib.rs` are built from the
//! plain types here in one place, in `engine.rs`.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ferry_core::limits::MAX_PATH_LEN;
use ferry_core::wire::{Decoder, Encoder, WireError};

/// The version byte every day file starts with.
const FORMAT_VERSION: u8 = 1;

/// The most entries one day file may hold. See the module documentation.
const MAX_ENTRIES_PER_DAY: u32 = 10_000;

/// The most entries [`AccessLog::query`] may return, whatever `limit` asks.
const MAX_QUERY_LIMIT: u32 = 1_000;

/// How many days a day file is kept before [`AccessLog::prune`] removes it.
const RETENTION_DAYS: i64 = 30;

/// Seconds in one day, for turning a Unix time into a day index.
const SECS_PER_DAY: i64 = 86_400;

/// How many seconds a pending entry may sit untouched before [`RollUp::tick`]
/// finalises it.
const IDLE_SECS: i64 = 5;

/// The exact length of a device key rendered as lowercase hex.
const MAX_DEVICE_KEY_HEX_LEN: usize = 64;

/// A generous bound on one encoded entry's frame, well over the largest
/// entry this format can produce: 64 bytes of device key hex, two verb and
/// actor bytes, [`MAX_PATH_LEN`] bytes of path, three optional counts, and
/// an eight byte time. This only bounds how much a corrupt or hostile file
/// could make one read allocate; it is not a wire limit anyone negotiates.
const MAX_ENTRY_FRAME_BYTES: usize = 2048;

/// The most bytes a torn tail from a crash mid append can ever leave: one
/// frame's four byte length prefix plus its content, at the largest content
/// [`decode_day_file`] will ever accept. An undecodable remainder longer
/// than this is not a torn tail; it is damage somewhere the file's own
/// framing cannot explain, and [`AccessLog::next_sequence`] treats it
/// differently. See the module documentation, "Framing".
const MAX_TORN_TAIL_BYTES: usize = MAX_ENTRY_FRAME_BYTES + 4;

/// Why an access log operation failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccessLogError {
    /// The filesystem refused something.
    #[error("storage failed: {0}")]
    Io(#[from] io::Error),
    /// A day file's version byte does not name a format this build knows.
    #[error("day file format version {0} is not one this build knows")]
    UnknownFormat(u8),
}

/// One kind of file operation the access log records.
///
/// Mirrors `AccessVerb` in `docs/engine-contract.md`, item 13. `set_mtime`
/// has no member here: the contract says it is never logged, because it
/// always follows a write that already is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessVerb {
    /// A folder listing. One entry covers every page of it.
    List,
    /// A single file or folder's metadata.
    Stat,
    /// Bytes read from a file.
    Read,
    /// Bytes written to a file.
    Write,
    /// A file shortened or extended to a given length.
    Truncate,
    /// A file or folder renamed.
    Rename,
    /// A folder created.
    Mkdir,
    /// A file or folder removed.
    Delete,
}

impl AccessVerb {
    /// The one byte this verb is stored as.
    fn to_byte(self) -> u8 {
        match self {
            Self::List => 0,
            Self::Stat => 1,
            Self::Read => 2,
            Self::Write => 3,
            Self::Truncate => 4,
            Self::Rename => 5,
            Self::Mkdir => 6,
            Self::Delete => 7,
        }
    }

    /// The verb a stored byte names, or `None` for a byte no format version
    /// this build knows ever wrote.
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::List,
            1 => Self::Stat,
            2 => Self::Read,
            3 => Self::Write,
            4 => Self::Truncate,
            5 => Self::Rename,
            6 => Self::Mkdir,
            7 => Self::Delete,
            _ => return None,
        })
    }
}

/// Who performed the operation.
///
/// Mirrors `Actor` in `docs/engine-contract.md`, item 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Actor {
    /// The paired device, acting on this device's files.
    Peer,
    /// This device, acting on the paired device's files.
    This,
}

impl Actor {
    /// The one byte this actor is stored as.
    fn to_byte(self) -> u8 {
        match self {
            Self::Peer => 0,
            Self::This => 1,
        }
    }

    /// The actor a stored byte names, or `None` for an unknown byte.
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Peer,
            1 => Self::This,
            _ => return None,
        })
    }
}

/// What one access log entry says happened, other than when and where in the
/// day file it landed.
///
/// This is not the wire shape and carries no `uniffi` derive. It exists so
/// [`AccessLog::append`] and [`RollUp::touch`] each take one small value
/// instead of eight loose parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryFields {
    /// The paired device this entry is about, as 64 lowercase hex
    /// characters.
    pub(crate) device_key_hex: String,
    /// Who performed the operation.
    pub(crate) actor: Actor,
    /// Which kind of operation.
    pub(crate) verb: AccessVerb,
    /// Root-relative, beginning with the root name: `"Desktop/Q3 notes.md"`.
    pub(crate) path: String,
    /// Bytes moved, for a read or a write.
    pub(crate) bytes: Option<u64>,
    /// How many entries a listing returned.
    pub(crate) entries: Option<u32>,
    /// How many files a folder copy covered.
    pub(crate) files: Option<u32>,
}

/// One access log entry, as [`AccessLog::query`] returns it.
///
/// Mirrors `AccessEntry` in `docs/engine-contract.md`, item 13, field for
/// field. The `uniffi::Record` the app sees is built from this later, on the
/// boundary; this type carries no `uniffi` derive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// `"<day>-<sequence>"`. Stable across restarts.
    pub(crate) id: String,
    /// The paired device this entry is about, as 64 lowercase hex
    /// characters.
    pub(crate) device_key_hex: String,
    /// Who performed the operation.
    pub(crate) actor: Actor,
    /// Which kind of operation.
    pub(crate) verb: AccessVerb,
    /// Root-relative, beginning with the root name.
    pub(crate) path: String,
    /// Bytes moved, for a read or a write.
    pub(crate) bytes: Option<u64>,
    /// How many entries a listing returned.
    pub(crate) entries: Option<u32>,
    /// How many files a folder copy covered.
    pub(crate) files: Option<u32>,
    /// When the operation this entry describes first happened.
    pub(crate) at_unix_secs: i64,
}

/// One entry as read back from a day file, before it is given an id.
///
/// A day file holds these in write order, so an entry's id is always its
/// position among them; nothing here needs to carry that position itself.
struct StoredEntry {
    fields: EntryFields,
    at_unix_secs: i64,
}

/// The year, month, and day, in the civil (Gregorian) calendar, that `days`
/// whole days after 1970-01-01 falls on.
///
/// This crate carries no date library, so this is the small, well known
/// integer algorithm Howard Hinnant published for exactly this conversion,
/// rather than a new dependency this build could not add anyway. It is used
/// for exactly one thing: the `YYYYMMDD` name of a day file.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    // `month` is always in [1, 12] and `day` always in [1, 31]; the fallback
    // can never run, but nothing here may unwrap.
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// The UTC calendar day `unix_secs` falls on, as the exact eight ASCII
/// digits a day file is named with: `"YYYYMMDD"`.
///
/// This is the one place this module turns a number into a date, and it
/// exists only because the file name has to be exactly this shape.
fn day_key(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(SECS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}{month:02}{day:02}")
}

/// `Encoder` has no signed integer method, so a Unix second count is carried
/// as its bit pattern. `ferry-core`'s `peers.rs` does the same, for the same
/// reason.
fn encode_i64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

/// The inverse of [`encode_i64`].
fn decode_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

/// Write an optional byte count as a presence byte followed by the value, or
/// a zero value when there is none.
fn encode_option_u64(encoder: &mut Encoder, value: Option<u64>) {
    if let Some(value) = value {
        encoder.u8(1);
        encoder.u64(value);
    } else {
        encoder.u8(0);
        encoder.u64(0);
    }
}

/// The inverse of [`encode_option_u64`]. The outer `Result` is decode
/// failure (not enough bytes left); the inner `Option` is the value itself.
fn decode_option_u64(decoder: &mut Decoder<'_>) -> Result<Option<u64>, WireError> {
    let present = decoder.u8()?;
    let value = decoder.u64()?;
    Ok(if present == 0 { None } else { Some(value) })
}

/// Write an optional item count the same way [`encode_option_u64`] writes a
/// byte count.
fn encode_option_u32(encoder: &mut Encoder, value: Option<u32>) {
    if let Some(value) = value {
        encoder.u8(1);
        encoder.u32(value);
    } else {
        encoder.u8(0);
        encoder.u32(0);
    }
}

/// The inverse of [`encode_option_u32`]. See [`decode_option_u64`] for what
/// the outer `Result` and inner `Option` each mean.
fn decode_option_u32(decoder: &mut Decoder<'_>) -> Result<Option<u32>, WireError> {
    let present = decoder.u8()?;
    let value = decoder.u32()?;
    Ok(if present == 0 { None } else { Some(value) })
}

/// Add two optional counts together. Treats a missing side as zero, so the
/// result is `None` only when both sides are.
fn add_option_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value),
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
    }
}

/// The `u32` twin of [`add_option_u64`].
fn add_option_u32(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value),
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
    }
}

/// Encode one entry's fields and its time as the bytes that go inside one
/// frame. The id is never part of this: see [`StoredEntry`].
fn encode_entry(fields: &EntryFields, at_unix_secs: i64) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.text(&fields.device_key_hex);
    encoder.u8(fields.actor.to_byte());
    encoder.u8(fields.verb.to_byte());
    encoder.text(&fields.path);
    encode_option_u64(&mut encoder, fields.bytes);
    encode_option_u32(&mut encoder, fields.entries);
    encode_option_u32(&mut encoder, fields.files);
    encoder.u64(encode_i64(at_unix_secs));
    encoder.finish()
}

/// The inverse of [`encode_entry`]. `None` means the bytes do not decode as
/// one whole entry, which is exactly what a truncated last frame looks like.
fn decode_entry(bytes: &[u8]) -> Option<StoredEntry> {
    let mut decoder = Decoder::new(bytes);
    let device_key_hex = decoder.text(MAX_DEVICE_KEY_HEX_LEN).ok()?.to_owned();
    let actor = Actor::from_byte(decoder.u8().ok()?)?;
    let verb = AccessVerb::from_byte(decoder.u8().ok()?)?;
    let path = decoder.text(MAX_PATH_LEN).ok()?.to_owned();
    let bytes_field = decode_option_u64(&mut decoder).ok()?;
    let entries_field = decode_option_u32(&mut decoder).ok()?;
    let files_field = decode_option_u32(&mut decoder).ok()?;
    let at_unix_secs = decode_i64(decoder.u64().ok()?);
    decoder.finish().ok()?;
    Some(StoredEntry {
        fields: EntryFields {
            device_key_hex,
            actor,
            verb,
            path,
            bytes: bytes_field,
            entries: entries_field,
            files: files_field,
        },
        at_unix_secs,
    })
}

/// Decode every whole entry in one day file's bytes, in the order they were
/// written, alongside how many leading bytes of the file those whole
/// entries take up.
///
/// A frame whose declared length runs past the end of `bytes`, or whose
/// content does not decode cleanly, stops the read where it is rather than
/// failing it: on a day file that can only be an unfinished last write, and
/// every entry before it is returned. The byte count is always the header
/// plus exactly the whole frames returned, never including a part of the
/// unfinished tail, so a caller that wants to repair the file on disk knows
/// precisely where to cut it. An empty file reads as a day with no entries
/// yet, at length zero.
///
/// # Errors
///
/// Returns [`AccessLogError::UnknownFormat`] when the file has bytes but its
/// first one is not [`FORMAT_VERSION`].
fn decode_day_file(bytes: &[u8]) -> Result<(Vec<StoredEntry>, usize), AccessLogError> {
    if bytes.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if bytes[0] != FORMAT_VERSION {
        return Err(AccessLogError::UnknownFormat(bytes[0]));
    }
    let mut decoder = Decoder::new(&bytes[1..]);
    let mut out = Vec::new();
    let mut good_len = 1; // the header byte
    loop {
        let before = decoder.remaining();
        if before == 0 {
            break;
        }
        let Ok(frame) = decoder.bytes(MAX_ENTRY_FRAME_BYTES) else {
            break;
        };
        let Some(entry) = decode_entry(frame) else {
            break;
        };
        out.push(entry);
        good_len += before - decoder.remaining();
    }
    Ok((out, good_len))
}

/// Whether `name` is exactly eight ASCII digits, the shape of a day file's
/// name.
fn is_day_file_name(name: &str) -> bool {
    name.len() == 8 && name.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `name` is a day file set aside as damaged: an eight digit day
/// followed by `.damaged`. See [`AccessLog::next_sequence`].
fn is_damaged_file_name(name: &str) -> bool {
    name.strip_suffix(".damaged").is_some_and(is_day_file_name)
}

/// Restrict `dir` to this account only, the way `ferry-core`'s `peers.rs`
/// restricts the files it writes. The access log names every path a paired
/// device has touched, so the folder it lives in gets the same treatment as
/// a secret key: readable and writable by nobody else.
#[cfg(unix)]
fn set_dir_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_dir_private(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Open `path` in append mode, creating it if it is not there yet, in mode
/// `0o600` on Unix so a day file is readable only by this account. The mode
/// is set as part of the same syscall that creates the file, as
/// `ferry-core`'s `peers.rs` does for a secret key, so there is no moment
/// where a fresh day file exists with a wider mode. Unlike a secret key, a
/// day file is opened many times as the day goes on, so this cannot use
/// `create_new`: reopening an existing file leaves its mode exactly as it
/// was, which is what an append needs.
#[cfg(unix)]
fn open_day_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_day_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().append(true).create(true).open(path)
}

/// The access log, one file per UTC day, under `<data_dir>/access_log/`.
///
/// See the module documentation for the on-disk framing and the retention
/// rule. Every method that changes the store takes `&mut self`; nothing here
/// is safe to share across threads on its own, the same as
/// `ferry_core::peers::PeerStore`. A future caller that needs that puts a
/// `Mutex` around it, as `state.rs` does for the engine's other state.
#[derive(Debug)]
pub(crate) struct AccessLog {
    dir: PathBuf,
    /// The next sequence number to hand out for a day, once that day has
    /// been looked at. Filled in lazily, the first time a day is appended
    /// to or found already on disk, and kept from then on so appending
    /// never rescans a day file it already knows.
    sequences: HashMap<String, u32>,
}

impl AccessLog {
    /// Open the store rooted at `<data_dir>/access_log/`, creating that
    /// folder if it is not there yet.
    ///
    /// # Errors
    ///
    /// Returns [`AccessLogError::Io`] when the folder cannot be created.
    pub(crate) fn open(data_dir: &Path) -> Result<Self, AccessLogError> {
        let dir = data_dir.join("access_log");
        fs::create_dir_all(&dir)?;
        set_dir_private(&dir)?;
        Ok(Self {
            dir,
            sequences: HashMap::new(),
        })
    }

    /// The path of the day file `day` (an eight digit `"YYYYMMDD"` string)
    /// belongs in.
    fn day_path(&self, day: &str) -> PathBuf {
        self.dir.join(day)
    }

    /// Where `day`'s file is moved when [`AccessLog::next_sequence`] finds
    /// more wrong with it than a torn tail.
    fn damaged_path(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{day}.damaged"))
    }

    /// The sequence number the next entry appended to `day` would get,
    /// learning it from the file on disk the first time `day` is asked
    /// about and caching it after that.
    ///
    /// A day file is only ever appended to, so the one way a crash can leave
    /// it wrong is a single unfinished frame at the very end; this cuts the
    /// file back to its last whole entry when that is all the undecodable
    /// remainder can be, so the next append lands cleanly after it rather
    /// than after garbage. A remainder longer than one frame is not that: it
    /// is damage the framing cannot explain, and truncating there would
    /// throw away whole entries that happen to follow it, entries which
    /// [`decode_day_file`] simply has no way to reach once it has stopped at
    /// the bad one before them. That file is set aside as `<day>.damaged`
    /// instead, kept for a person to look at, and a fresh file starts under
    /// `day`'s own name so new entries keep landing somewhere readable.
    fn next_sequence(&mut self, day: &str) -> Result<u32, AccessLogError> {
        if let Some(&next) = self.sequences.get(day) {
            return Ok(next);
        }
        let path = self.day_path(day);
        let count = match fs::read(&path) {
            Ok(bytes) => {
                let (entries, good_len) = decode_day_file(&bytes)?;
                let bad_len = bytes.len() - good_len;
                if bad_len == 0 {
                    entries.len()
                } else if bad_len <= MAX_TORN_TAIL_BYTES {
                    let file = fs::OpenOptions::new().write(true).open(&path)?;
                    file.set_len(u64::try_from(good_len).unwrap_or(u64::MAX))?;
                    entries.len()
                } else {
                    fs::rename(&path, self.damaged_path(day))?;
                    0
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => return Err(AccessLogError::Io(err)),
        };
        let next = u32::try_from(count).unwrap_or(u32::MAX).saturating_add(1);
        self.sequences.insert(day.to_owned(), next);
        Ok(next)
    }

    /// Append one entry, dated `at_unix_secs`, to its day's file.
    ///
    /// Returns `Ok(true)` when the entry was written, and `Ok(false)`
    /// without writing anything when that day already holds
    /// [`MAX_ENTRIES_PER_DAY`] entries.
    ///
    /// # Errors
    ///
    /// Returns [`AccessLogError`] when the day's existing file cannot be
    /// read to learn the next sequence, or when the write itself fails.
    pub(crate) fn append(
        &mut self,
        at_unix_secs: i64,
        fields: &EntryFields,
    ) -> Result<bool, AccessLogError> {
        let day = day_key(at_unix_secs);
        let next_sequence = self.next_sequence(&day)?;
        if next_sequence > MAX_ENTRIES_PER_DAY {
            return Ok(false);
        }
        let path = self.day_path(&day);
        let needs_header = fs::metadata(&path)
            .map(|meta| meta.len() == 0)
            .unwrap_or(true);
        let mut frame = Encoder::new();
        if needs_header {
            frame.u8(FORMAT_VERSION);
        }
        frame.bytes(&encode_entry(fields, at_unix_secs));
        let mut file = open_day_file(&path)?;
        file.write_all(&frame.finish())?;
        self.sequences.insert(day, next_sequence + 1);
        Ok(true)
    }

    /// Every day file's name, newest day first.
    ///
    /// A day file's name is eight ASCII digits, so ordinary string order is
    /// chronological order; nothing here parses a date to sort them.
    fn day_files_newest_first(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_day_file_name(name))
            .collect();
        names.sort_unstable_by(|a, b| b.cmp(a));
        names
    }

    /// Every entry across every device, newest first, or only `device_key_hex`'s
    /// when it is `Some`.
    ///
    /// Reads day files from newest to oldest and stops once `limit` entries
    /// have been found. `limit` is capped at [`MAX_QUERY_LIMIT`]. A day file
    /// that cannot be read or does not decode is skipped rather than failing
    /// the whole call: a person asking what a device did should still see
    /// every day that is readable.
    #[must_use]
    pub(crate) fn query(&self, device_key_hex: Option<&str>, limit: u32) -> Vec<Entry> {
        let limit = limit.min(MAX_QUERY_LIMIT) as usize;
        let mut out = Vec::new();
        for day in self.day_files_newest_first() {
            if out.len() >= limit {
                break;
            }
            let Ok(bytes) = fs::read(self.day_path(&day)) else {
                continue;
            };
            let Ok((entries, _)) = decode_day_file(&bytes) else {
                continue;
            };
            for (index, stored) in entries.iter().enumerate().rev() {
                if let Some(filter) = device_key_hex
                    && stored.fields.device_key_hex != filter
                {
                    continue;
                }
                out.push(Entry {
                    id: format!("{day}-{}", index + 1),
                    device_key_hex: stored.fields.device_key_hex.clone(),
                    actor: stored.fields.actor,
                    verb: stored.fields.verb,
                    path: stored.fields.path.clone(),
                    bytes: stored.fields.bytes,
                    entries: stored.fields.entries,
                    files: stored.fields.files,
                    at_unix_secs: stored.at_unix_secs,
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    /// Every damaged day file's name (see [`AccessLog::next_sequence`]), in
    /// no particular order: [`AccessLog::prune`] only checks each one's own
    /// age, never sequences through them the way it does ordinary day files.
    fn damaged_file_names(&self) -> Vec<String> {
        fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_damaged_file_name(name))
            .collect()
    }

    /// Delete every day file, and every day file set aside as damaged, older
    /// than [`RETENTION_DAYS`] days, as measured from `now`.
    ///
    /// A file exactly [`RETENTION_DAYS`] days old is kept: it is not yet
    /// older than the limit. A damaged file's age is the day named in its
    /// own file name, the day it was writing to when it was set aside, not
    /// the day it happened to be pruned.
    ///
    /// # Errors
    ///
    /// Returns [`AccessLogError::Io`] when a file that should be removed
    /// cannot be.
    pub(crate) fn prune(&mut self, now: i64) -> Result<(), AccessLogError> {
        let cutoff = day_key(now - RETENTION_DAYS * SECS_PER_DAY);
        for name in self.day_files_newest_first() {
            if name < cutoff {
                fs::remove_file(self.day_path(&name))?;
                self.sequences.remove(&name);
            }
        }
        for name in self.damaged_file_names() {
            let day = name.strip_suffix(".damaged").unwrap_or(&name);
            if day < cutoff.as_str() {
                fs::remove_file(self.dir.join(&name))?;
            }
        }
        Ok(())
    }
}

/// One access log entry that is still open: more operations on the same
/// connection, verb, and path may still add to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    fields: EntryFields,
    /// When this entry's first operation happened. What it is filed under
    /// once it is written to the store.
    first_touch: i64,
    /// When this entry's most recent operation happened. Compared against
    /// `now` in [`RollUp::tick`] to decide whether it has gone idle.
    last_touch: i64,
}

/// Write `pending` to `store` and note in `changed` whether it actually was.
///
/// A free function rather than a method, so it can be called while a
/// separate mutable borrow of `RollUp`'s pending map is still alive: it only
/// ever touches the two fields it is given, never the map.
fn finalize_pending(store: &mut AccessLog, changed: &mut bool, pending: &Pending) {
    if matches!(store.append(pending.first_touch, &pending.fields), Ok(true)) {
        *changed = true;
    }
}

/// Groups operations on one connection into access log entries, and owns the
/// store they are written to.
///
/// See the module documentation for the rule that decides when a pending
/// entry becomes final. A dropped write to the store (a filesystem error, or
/// a day already at [`MAX_ENTRIES_PER_DAY`]) loses that one entry rather
/// than surfacing an error here: the log is a record for a person to read
/// later, not a path anything else depends on, so one lost entry should
/// never be allowed to break the connection it was describing.
#[derive(Debug)]
pub(crate) struct RollUp {
    store: AccessLog,
    /// Pending entries, keyed by connection. Each connection's list holds at
    /// most one entry per distinct path, and at most one per verb on that
    /// path.
    pending: HashMap<u64, Vec<Pending>>,
    /// Set the moment an entry becomes final, and cleared by
    /// [`RollUp::take_changed`].
    changed: bool,
}

impl RollUp {
    /// Wrap `store` with an empty roll-up.
    pub(crate) fn new(store: AccessLog) -> Self {
        Self {
            store,
            pending: HashMap::new(),
            changed: false,
        }
    }

    /// The store this roll-up writes finished entries to, for
    /// [`AccessLog::query`] and [`AccessLog::prune`].
    #[must_use]
    pub(crate) fn store(&self) -> &AccessLog {
        &self.store
    }

    /// Delete day files older than the retention window, from the store this
    /// roll-up writes to. See [`AccessLog::prune`].
    ///
    /// # Errors
    ///
    /// Returns [`AccessLogError::Io`] when a file that should be removed
    /// cannot be.
    pub(crate) fn prune(&mut self, now: i64) -> Result<(), AccessLogError> {
        self.store.prune(now)
    }

    /// Record one operation on `connection`.
    ///
    /// Merges into the pending entry for this connection's verb and path
    /// when there is one: bytes, entries, and files add up, and the entry's
    /// idle clock resets to `now`. Otherwise starts a new pending entry.
    /// Before either, every pending entry on this connection whose path is
    /// not `fields.path` becomes final, whatever its verb: a connection
    /// only has one path open at a time.
    pub(crate) fn touch(&mut self, now: i64, connection: u64, fields: EntryFields) {
        let list = self.pending.entry(connection).or_default();
        let mut index = 0;
        while index < list.len() {
            if list[index].fields.path == fields.path {
                index += 1;
            } else {
                let done = list.remove(index);
                finalize_pending(&mut self.store, &mut self.changed, &done);
            }
        }
        // Every entry left in `list` now shares `fields.path`, so only the
        // verb still needs to be matched.
        if let Some(existing) = list.iter_mut().find(|p| p.fields.verb == fields.verb) {
            existing.fields.bytes = add_option_u64(existing.fields.bytes, fields.bytes);
            existing.fields.entries = add_option_u32(existing.fields.entries, fields.entries);
            existing.fields.files = add_option_u32(existing.fields.files, fields.files);
            existing.last_touch = now;
        } else {
            list.push(Pending {
                fields,
                first_touch: now,
                last_touch: now,
            });
        }
    }

    /// Finalise every pending entry, on any connection, idle for
    /// [`IDLE_SECS`] seconds or more as of `now`.
    pub(crate) fn tick(&mut self, now: i64) {
        let mut done = Vec::new();
        for list in self.pending.values_mut() {
            let mut index = 0;
            while index < list.len() {
                if now - list[index].last_touch >= IDLE_SECS {
                    done.push(list.remove(index));
                } else {
                    index += 1;
                }
            }
        }
        self.pending.retain(|_, list| !list.is_empty());
        for pending in &done {
            finalize_pending(&mut self.store, &mut self.changed, pending);
        }
    }

    /// Finalise every pending entry on `connection`, because it has ended.
    ///
    /// Every entry is written under its own first touch time, which is
    /// already recorded, so `now` names nothing this needs; it is here so
    /// every roll-up method takes the current time the same way.
    pub(crate) fn connection_ended(&mut self, now: i64, connection: u64) {
        let _ = now;
        if let Some(list) = self.pending.remove(&connection) {
            for pending in &list {
                finalize_pending(&mut self.store, &mut self.changed, pending);
            }
        }
    }

    /// Whether an entry has become final since the last call, clearing the
    /// flag either way.
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Finalise every pending entry, on every connection, whatever its idle
    /// time. Called once, at `stop`, right before the roll-up is dropped.
    /// A calling-side operation always finalises itself in the same breath
    /// it touches this roll-up, so it never leaves anything behind; what is
    /// still pending here comes from a served connection whose thread `stop`
    /// cannot join (`lib.rs`, "a serving thread cannot be woken") and that
    /// has not yet gone idle or ended on its own. Quitting must not lose it.
    ///
    /// `now` names nothing this needs, for the same reason it names nothing
    /// in [`RollUp::connection_ended`]: every pending entry already carries
    /// its own first touch time to file under.
    pub(crate) fn finalize_all(&mut self, now: i64) {
        let _ = now;
        for list in std::mem::take(&mut self.pending).into_values() {
            for pending in &list {
                finalize_pending(&mut self.store, &mut self.changed, pending);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessLog, AccessLogError, AccessVerb, Actor, Entry, EntryFields, RollUp};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Every test gets its own folder, so tests running in parallel never
    /// share one. Mirrors `record.rs` and `ferry-core`'s `peers.rs`.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ferry-access-test-{}-{label}-{n}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("a folder for the test");
        dir
    }

    /// One arbitrary but fixed moment, so tests do not depend on the clock.
    /// 2026-03-01T00:00:00Z.
    const BASE_TIME: i64 = 1_772_323_200;

    fn fields(device: &str, verb: AccessVerb, path: &str, bytes: Option<u64>) -> EntryFields {
        EntryFields {
            device_key_hex: device.to_owned(),
            actor: Actor::This,
            verb,
            path: path.to_owned(),
            bytes,
            entries: None,
            files: None,
        }
    }

    /// One entry, framed exactly as [`AccessLog::append`] would write it: a
    /// four byte big endian length, then [`super::encode_entry`]'s bytes.
    /// Lets a test build a day file by hand, byte for byte, to put something
    /// in the middle of it that only `AccessLog::append` would never write.
    fn frame_bytes(fields: &EntryFields, at_unix_secs: i64) -> Vec<u8> {
        let content = super::encode_entry(fields, at_unix_secs);
        let mut frame = u32::try_from(content.len())
            .expect("a test entry's content fits in a u32")
            .to_be_bytes()
            .to_vec();
        frame.extend_from_slice(&content);
        frame
    }

    #[test]
    fn a_flipped_byte_in_the_middle_sets_the_day_file_aside_and_a_later_append_still_works() {
        let dir = temp_dir("damaged-middle");
        let access_log_dir = dir.join("access_log");
        fs::create_dir_all(&access_log_dir).expect("the access log folder should be makeable");
        let day = super::day_key(BASE_TIME);
        let day_path = access_log_dir.join(&day);

        // Two large paths, so the good entry that follows the corrupted one
        // is big enough that losing it, on top of the corrupted frame
        // itself, is more than one frame's worth of bytes: exactly the case
        // that must not be treated as an ordinary torn tail.
        let big_path = "x".repeat(1024);
        let one = fields("device", AccessVerb::Read, "one", Some(1));
        let two = fields("device", AccessVerb::Read, &big_path, Some(2));
        let three = fields("device", AccessVerb::Read, &big_path, Some(3));

        let frame_one = frame_bytes(&one, BASE_TIME);
        let mut frame_two = frame_bytes(&two, BASE_TIME + 1);
        // The actor byte sits right after the device key's four byte length
        // prefix and its text. No `Actor` variant is ever stored as 0xFF, so
        // this alone makes the whole frame fail to decode, the way a single
        // flipped bit on disk would.
        let actor_offset = 4 + "device".len() + 4;
        frame_two[actor_offset] = 0xFF;
        let frame_three = frame_bytes(&three, BASE_TIME + 2);

        let mut bytes = vec![super::FORMAT_VERSION];
        bytes.extend_from_slice(&frame_one);
        bytes.extend_from_slice(&frame_two);
        bytes.extend_from_slice(&frame_three);
        fs::write(&day_path, &bytes).expect("the hand built day file should write");

        let mut log = AccessLog::open(&dir).expect("the store should open");
        let wrote = log
            .append(
                BASE_TIME + 3,
                &fields("device", AccessVerb::Read, "four", Some(4)),
            )
            .expect("appending after a damaged day file should still work");
        assert!(wrote);

        let damaged_path = access_log_dir.join(format!("{day}.damaged"));
        assert!(
            damaged_path.exists(),
            "the damaged file should be set aside rather than truncated"
        );
        assert_eq!(
            fs::read(&damaged_path).expect("the damaged file should be readable"),
            bytes,
            "the damaged file keeps exactly the bytes that were there, good entries included"
        );

        let found = log.query(None, 10);
        assert_eq!(
            found.len(),
            1,
            "only the entry appended after the file was set aside is readable"
        );
        assert_eq!(found[0].path, "four");
    }

    #[test]
    fn append_then_query_round_trips_one_entry() {
        let dir = temp_dir("round-trip");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        let entry = fields(
            "a".repeat(64).as_str(),
            AccessVerb::Read,
            "Desktop/a.txt",
            Some(10),
        );

        let wrote = log
            .append(BASE_TIME, &entry)
            .expect("the append should succeed");
        assert!(wrote, "the entry should have been written");

        let found = log.query(None, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].device_key_hex, "a".repeat(64));
        assert_eq!(found[0].verb, AccessVerb::Read);
        assert_eq!(found[0].path, "Desktop/a.txt");
        assert_eq!(found[0].bytes, Some(10));
        assert_eq!(found[0].at_unix_secs, BASE_TIME);
        assert!(found[0].id.starts_with(&super::day_key(BASE_TIME)));
    }

    #[test]
    #[cfg(unix)]
    fn on_unix_the_folder_and_its_day_files_are_created_readable_by_this_account_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("permissions");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        log.append(
            BASE_TIME,
            &fields("device", AccessVerb::Read, "one", Some(1)),
        )
        .expect("the append should succeed");

        let access_log_dir = dir.join("access_log");
        let dir_mode = fs::metadata(&access_log_dir)
            .expect("the folder should exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "the access log folder is private");

        let day_path = access_log_dir.join(super::day_key(BASE_TIME));
        let file_mode = fs::metadata(&day_path)
            .expect("the day file should exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "a day file is private");
    }

    #[test]
    fn newest_entries_come_first_across_two_days() {
        let dir = temp_dir("two-days");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        let one_day = 86_400;

        log.append(
            BASE_TIME,
            &fields("device", AccessVerb::List, "Desktop", None),
        )
        .expect("the first append should succeed");
        log.append(
            BASE_TIME + one_day,
            &fields("device", AccessVerb::List, "Downloads", None),
        )
        .expect("the second append should succeed");

        let found = log.query(None, 10);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "Downloads", "the later day comes first");
        assert_eq!(found[1].path, "Desktop");
    }

    #[test]
    fn a_device_filter_returns_only_that_devices_entries() {
        let dir = temp_dir("device-filter");
        let mut log = AccessLog::open(&dir).expect("the store should open");

        log.append(
            BASE_TIME,
            &fields("11".repeat(32).as_str(), AccessVerb::Read, "a", Some(1)),
        )
        .expect("the append should succeed");
        log.append(
            BASE_TIME + 1,
            &fields("22".repeat(32).as_str(), AccessVerb::Read, "b", Some(2)),
        )
        .expect("the append should succeed");

        let found = log.query(Some("11".repeat(32).as_str()), 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "a");
    }

    #[test]
    fn query_never_returns_more_than_the_1000_cap() {
        let dir = temp_dir("query-cap");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        for i in 0..1100i64 {
            log.append(
                BASE_TIME + i,
                &fields("device", AccessVerb::Stat, "f", None),
            )
            .expect("the append should succeed");
        }

        let found = log.query(None, u32::MAX);
        assert_eq!(found.len(), 1000);
    }

    #[test]
    fn a_full_day_refuses_a_further_append() {
        let dir = temp_dir("day-cap");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        // Seed the cache directly rather than writing 10,000 real entries,
        // which this test does not need to prove the cap. A cached next
        // sequence of 10,001 means the day already holds 10,000 entries.
        let day = super::day_key(BASE_TIME);
        log.sequences.insert(day, 10_001);

        let wrote = log
            .append(BASE_TIME, &fields("device", AccessVerb::Read, "f", Some(1)))
            .expect("append should not error even when the day is full");
        assert!(!wrote, "the day already holds 10,000 entries");

        assert!(
            log.query(None, 10).is_empty(),
            "nothing should have been written"
        );
    }

    #[test]
    fn prune_removes_a_31_day_old_file_and_keeps_a_29_day_old_one() {
        let dir = temp_dir("prune");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        let one_day = 86_400;

        log.append(
            BASE_TIME - 31 * one_day,
            &fields("device", AccessVerb::Read, "old", Some(1)),
        )
        .expect("the append should succeed");
        log.append(
            BASE_TIME - 29 * one_day,
            &fields("device", AccessVerb::Read, "recent", Some(1)),
        )
        .expect("the append should succeed");

        log.prune(BASE_TIME).expect("prune should succeed");

        let found = log.query(None, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "recent");
    }

    #[test]
    fn a_truncated_last_entry_is_skipped_and_a_later_append_still_works() {
        let dir = temp_dir("truncated");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        log.append(
            BASE_TIME,
            &fields("device", AccessVerb::Read, "one", Some(1)),
        )
        .expect("the first append should succeed");
        log.append(
            BASE_TIME + 1,
            &fields("device", AccessVerb::Read, "two", Some(1)),
        )
        .expect("the second append should succeed");

        // Cut into the last frame, as a crash mid write would.
        let day = super::day_key(BASE_TIME);
        let path = dir.join("access_log").join(&day);
        let mut bytes = fs::read(&path).expect("the day file should be readable");
        bytes.truncate(bytes.len() - 2);
        fs::write(&path, &bytes).expect("the truncated file should write");

        // A fresh store, so nothing is left over in an in-memory cache.
        let mut reopened = AccessLog::open(&dir).expect("the store should reopen");
        let found = reopened.query(None, 10);
        assert_eq!(found.len(), 1, "only the whole first entry survives");
        assert_eq!(found[0].path, "one");

        let wrote = reopened
            .append(
                BASE_TIME + 2,
                &fields("device", AccessVerb::Read, "three", Some(1)),
            )
            .expect("appending after a truncated tail should still work");
        assert!(wrote);

        let found = reopened.query(None, 10);
        assert_eq!(found.len(), 2, "both the old and the new entry are there");
        assert_eq!(found[0].path, "three", "the new entry is newest");
        assert_eq!(found[0].id, format!("{day}-2"), "sequence continues at 2");
    }

    #[test]
    fn sequence_continues_after_reopen() {
        let dir = temp_dir("reopen");
        {
            let mut log = AccessLog::open(&dir).expect("the store should open");
            log.append(
                BASE_TIME,
                &fields("device", AccessVerb::Read, "one", Some(1)),
            )
            .expect("the append should succeed");
            log.append(
                BASE_TIME + 1,
                &fields("device", AccessVerb::Read, "two", Some(1)),
            )
            .expect("the append should succeed");
        }

        let mut reopened = AccessLog::open(&dir).expect("the store should reopen");
        reopened
            .append(
                BASE_TIME + 2,
                &fields("device", AccessVerb::Read, "three", Some(1)),
            )
            .expect("the append should succeed");

        let day = super::day_key(BASE_TIME);
        let found = reopened.query(None, 10);
        assert_eq!(found[0].id, format!("{day}-3"));
    }

    #[test]
    fn a_bad_version_byte_is_reported_as_unknown_format() {
        let dir = temp_dir("bad-version");
        let mut log = AccessLog::open(&dir).expect("the store should open");
        log.append(
            BASE_TIME,
            &fields("device", AccessVerb::Read, "one", Some(1)),
        )
        .expect("the append should succeed");

        let day = super::day_key(BASE_TIME);
        let path = dir.join("access_log").join(&day);
        let mut bytes = fs::read(&path).expect("the day file should be readable");
        bytes[0] = 99;
        fs::write(&path, &bytes).expect("the corrupted file should write");

        let mut reopened = AccessLog::open(&dir).expect("the store should reopen");
        let result = reopened.append(
            BASE_TIME + 1,
            &fields("device", AccessVerb::Read, "two", Some(1)),
        );
        assert!(matches!(result, Err(AccessLogError::UnknownFormat(99))));
        assert!(
            reopened.query(None, 10).is_empty(),
            "query treats the same file as unreadable and skips it"
        );
    }

    fn rollup() -> RollUp {
        let dir = temp_dir("rollup");
        RollUp::new(AccessLog::open(&dir).expect("the store should open"))
    }

    fn touch_of(
        rollup: &mut RollUp,
        now: i64,
        connection: u64,
        verb: AccessVerb,
        path: &str,
        bytes: Option<u64>,
    ) {
        rollup.touch(now, connection, fields("device", verb, path, bytes));
    }

    fn one_entry(rollup: &RollUp) -> Entry {
        let found = rollup.store().query(None, 10);
        assert_eq!(found.len(), 1, "exactly one entry should be final");
        found.into_iter().next().expect("checked above")
    }

    #[test]
    fn two_reads_of_one_path_merge_into_one_entry_with_summed_bytes() {
        let mut rollup = rollup();
        touch_of(&mut rollup, BASE_TIME, 1, AccessVerb::Read, "a", Some(10));
        touch_of(
            &mut rollup,
            BASE_TIME + 1,
            1,
            AccessVerb::Read,
            "a",
            Some(5),
        );

        assert!(
            rollup.store().query(None, 10).is_empty(),
            "nothing is final yet"
        );

        rollup.connection_ended(BASE_TIME + 1, 1);

        let entry = one_entry(&rollup);
        assert_eq!(entry.bytes, Some(15));
        assert_eq!(entry.at_unix_secs, BASE_TIME, "filed under the first touch");
        assert!(rollup.take_changed());
    }

    #[test]
    fn touching_a_different_path_finalises_the_old_one() {
        let mut rollup = rollup();
        touch_of(&mut rollup, BASE_TIME, 1, AccessVerb::Read, "a", Some(10));
        touch_of(
            &mut rollup,
            BASE_TIME + 1,
            1,
            AccessVerb::Read,
            "b",
            Some(20),
        );

        let found = rollup.store().query(None, 10);
        assert_eq!(found.len(), 1, "only the entry on the old path is final");
        assert_eq!(found[0].path, "a");
    }

    #[test]
    fn an_idle_entry_is_finalised_by_tick() {
        let mut rollup = rollup();
        touch_of(&mut rollup, BASE_TIME, 1, AccessVerb::Read, "a", Some(10));

        rollup.tick(BASE_TIME + 4);
        assert!(
            rollup.store().query(None, 10).is_empty(),
            "four seconds have not passed the five second idle mark"
        );

        rollup.tick(BASE_TIME + 5);
        assert_eq!(rollup.store().query(None, 10).len(), 1);
    }

    #[test]
    fn connection_ended_finalises_every_pending_entry_on_it() {
        let mut rollup = rollup();
        touch_of(&mut rollup, BASE_TIME, 1, AccessVerb::Stat, "a", None);
        touch_of(&mut rollup, BASE_TIME, 1, AccessVerb::Read, "a", Some(1));

        rollup.connection_ended(BASE_TIME, 1);

        assert_eq!(
            rollup.store().query(None, 10).len(),
            2,
            "both the stat and the read on the same path are separate entries"
        );
    }

    #[test]
    fn take_changed_clears_after_being_read() {
        let mut rollup = rollup();
        touch_of(&mut rollup, BASE_TIME, 1, AccessVerb::Read, "a", Some(1));
        rollup.connection_ended(BASE_TIME, 1);

        assert!(rollup.take_changed());
        assert!(!rollup.take_changed(), "the flag was already taken");
    }
}
