//! Turning one entry, and one whole day file, into bytes and back.
//!
//! Moved out of `access.rs` to keep that file to the store and the roll-up.
//! Nothing here changed when it moved: every function keeps its name, its
//! signature, and its doc comment.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use ferry_core::limits::MAX_PATH_LEN;
use ferry_core::wire::{Decoder, Encoder, WireError, decode_i64, encode_i64};

use super::{
    AccessLogError, AccessVerb, Actor, EntryFields, FORMAT_VERSION, MAX_DEVICE_KEY_HEX_LEN,
    MAX_ENTRY_FRAME_BYTES, SECS_PER_DAY,
};

/// One entry as read back from a day file, before it is given an id.
///
/// A day file holds these in write order, so an entry's id is always its
/// position among them; nothing here needs to carry that position itself.
pub(super) struct StoredEntry {
    pub(super) fields: EntryFields,
    pub(super) at_unix_secs: i64,
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
pub(super) fn day_key(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(SECS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}{month:02}{day:02}")
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
pub(super) fn add_option_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value),
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
    }
}

/// The `u32` twin of [`add_option_u64`].
pub(super) fn add_option_u32(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value),
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
    }
}

/// Encode one entry's fields and its time as the bytes that go inside one
/// frame. The id is never part of this: see [`StoredEntry`].
pub(super) fn encode_entry(fields: &EntryFields, at_unix_secs: i64) -> Vec<u8> {
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
pub(super) fn decode_day_file(bytes: &[u8]) -> Result<(Vec<StoredEntry>, usize), AccessLogError> {
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

/// How many of `entries` belong to each device, by `device_key_hex`.
///
/// `docs/audits/fable-security.md`, finding 2: [`AccessLog::load_day_state`]
/// calls this to seed [`DayState::entries_by_device`] from what a day file
/// already holds, so the per-device cap [`AccessLog::append`] enforces
/// counts correctly even for a day this build is only now loading from disk.
pub(super) fn device_counts(entries: &[StoredEntry]) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    for entry in entries {
        *counts
            .entry(entry.fields.device_key_hex.clone())
            .or_insert(0) += 1;
    }
    counts
}

/// Whether `name` is exactly eight ASCII digits, the shape of a day file's
/// name.
pub(super) fn is_day_file_name(name: &str) -> bool {
    name.len() == 8 && name.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `name` is a day file set aside as damaged: an eight digit day
/// followed by `.damaged`. See [`AccessLog::load_day_state`].
pub(super) fn is_damaged_file_name(name: &str) -> bool {
    name.strip_suffix(".damaged").is_some_and(is_day_file_name)
}

/// Restrict `dir` to this account only, the way `ferry-core`'s `peers.rs`
/// restricts the files it writes. The access log names every path a paired
/// device has touched, so the folder it lives in gets the same treatment as
/// a secret key: readable and writable by nobody else.
#[cfg(unix)]
pub(super) fn set_dir_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub(super) fn set_dir_private(_dir: &Path) -> io::Result<()> {
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
pub(super) fn open_day_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
pub(super) fn open_day_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().append(true).create(true).open(path)
}
