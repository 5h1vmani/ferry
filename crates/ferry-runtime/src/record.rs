//! The transfer record on disk, and the only way it is written.
//!
//! A record is what makes a transfer survive the app closing. It has two
//! shapes, because a transfer has two stages.
//!
//! A first pass record says where the file is coming from, how big it was
//! when the pass started, and how many whole chunks have arrived and been
//! hashed. It carries no manifest, because the manifest does not exist until
//! the whole file has been read once. See the module documentation of
//! `transfer.rs` for why the first pass builds it.
//!
//! A ready record holds the `Transfer` from `ferry-core`, manifest and all.
//! From then on every attempt is an ordinary resume.
//!
//! # Why the write goes through a temporary file
//!
//! `std::fs::write` truncates the file first and then writes. A process that
//! dies in the middle of that leaves a short file behind, and a record that
//! does not decode is skipped for ever: the transfer then starts again from
//! the first byte, or stops for good. So [`write_record`] hands the bytes to
//! `privatefile::write_atomic`, which writes them to a temporary name, flushes
//! them to disk, and only then renames the temporary file over the real one.
//! A rename is one step, so the real name always holds either the whole old
//! record or the whole new one.
//!
//! `ferry-core`'s `peers.rs` writes its own paired device list the same way,
//! but keeps its own copy of the pattern: that function is private to the
//! core crate, and this crate does not change the core.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use ferry_core::limits;
use ferry_core::path::RemotePath;
use ferry_core::session::Transfer;
use ferry_core::wire::{Decoder, Encoder};

use crate::errors::failed;
use crate::state::now_unix_secs;
use crate::{Direction, FerryError};

/// The version byte every record starts with.
///
/// Version 1 held no start time, end time, or direction. Version 2 added
/// those three but held no batch id. Both still load: see [`decode_meta`].
const FORMAT_VERSION: u8 = 3;

/// The stage byte of a record written during the first pass.
const STAGE_FIRST_PASS: u8 = 0;

/// The stage byte of a record that holds a manifest.
const STAGE_READY: u8 = 1;

/// A first pass that has not finished, as stored on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirstPass {
    /// Where the file lives on the other device.
    pub(crate) source: RemotePath,
    /// Where the file belongs here.
    pub(crate) destination: RemotePath,
    /// The size the source had when the pass started.
    pub(crate) source_size: u64,
    /// The modified time the source had when the pass started.
    pub(crate) source_mtime: i64,
    /// The chunk size the pass is using.
    pub(crate) chunk_size: u32,
    /// How many whole chunks have arrived and been hashed.
    pub(crate) chunks_done: u32,
}

impl FirstPass {
    /// How many bytes are on disk and hashed.
    pub(crate) fn bytes_done(&self) -> u64 {
        u64::from(self.chunks_done) * u64::from(self.chunk_size)
    }
}

/// The fields item 9 and item 4 add, common to both stages.
///
/// A version 1 record held none of these. Loading one back fills in
/// [`Meta::started_unix_secs`] from the record file's own modification
/// time, leaves [`Meta::ended_unix_secs`] `None`, and reports
/// [`Direction::Pull`]. See [`decode_meta`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Meta {
    /// When `pull` created the transfer this record belongs to.
    pub(crate) started_unix_secs: i64,
    /// When the transfer last finished or failed. A record this crate
    /// writes never carries `Some` here: the record is written while a
    /// transfer is in progress, and a finished transfer is removed rather
    /// than rewritten. The field still round-trips, for whatever writes
    /// one later.
    pub(crate) ended_unix_secs: Option<i64>,
    /// Which way the transfer moves the file.
    pub(crate) direction: Direction,
    /// Which batch this transfer belongs to, if `pull_folder` created it.
    /// A version 1 or version 2 record held no field for this and loads
    /// with `None`.
    pub(crate) batch_id: Option<String>,
}

/// One transfer record, in whichever stage it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Record {
    /// The manifest does not exist yet.
    FirstPass(Meta, FirstPass),
    /// The manifest is complete, so every later attempt is a resume.
    Ready(Meta, Transfer),
}

impl Record {
    /// The bytes to store.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION);
        match self {
            Self::FirstPass(meta, pass) => {
                e.u8(STAGE_FIRST_PASS);
                e.text(pass.source.as_str());
                e.text(pass.destination.as_str());
                e.u64(pass.source_size);
                e.fixed(&pass.source_mtime.to_be_bytes());
                e.u32(pass.chunk_size);
                e.u32(pass.chunks_done);
                encode_meta(&mut e, meta);
            }
            Self::Ready(meta, transfer) => {
                e.u8(STAGE_READY);
                e.bytes(&transfer.encode());
                encode_meta(&mut e, meta);
            }
        }
        e.finish()
    }

    /// Read a record back.
    ///
    /// `fallback_started_unix_secs` is used only for a version 1 record,
    /// which stored no start time of its own. The caller reads it from the
    /// record file's modification time.
    ///
    /// # Errors
    ///
    /// Returns a `ManifestError::Wire` code for bytes this build cannot
    /// read, which covers a record from a newer format and a record a disk
    /// damaged.
    pub(crate) fn decode(
        bytes: &[u8],
        fallback_started_unix_secs: i64,
    ) -> Result<Self, FerryError> {
        Self::decode_inner(bytes, fallback_started_unix_secs)
            .ok_or_else(|| failed("ManifestError::Wire"))
    }

    /// The body of [`Record::decode`], where every step may simply fail.
    fn decode_inner(bytes: &[u8], fallback_started_unix_secs: i64) -> Option<Self> {
        let mut d = Decoder::new(bytes);
        let version = d.u8().ok()?;
        if version == 0 || version > FORMAT_VERSION {
            return None;
        }
        let stage = d.u8().ok()?;
        let record = match stage {
            STAGE_FIRST_PASS => {
                let source = RemotePath::parse(d.text(limits::MAX_PATH_LEN).ok()?).ok()?;
                let destination = RemotePath::parse(d.text(limits::MAX_PATH_LEN).ok()?).ok()?;
                let source_size = d.u64().ok()?;
                let source_mtime = i64::from_be_bytes(d.fixed::<8>().ok()?);
                let chunk_size = d.u32().ok()?;
                let chunks_done = d.u32().ok()?;
                let meta = decode_meta(&mut d, version, fallback_started_unix_secs)?;
                Self::FirstPass(
                    meta,
                    FirstPass {
                        source,
                        destination,
                        source_size,
                        source_mtime,
                        chunk_size,
                        chunks_done,
                    },
                )
            }
            STAGE_READY => {
                let inner = d
                    .bytes(limits::MAX_MANIFEST_BYTES + 2 * limits::MAX_PATH_LEN + 64)
                    .ok()?;
                let transfer = Transfer::decode(inner).ok()?;
                let meta = decode_meta(&mut d, version, fallback_started_unix_secs)?;
                Self::Ready(meta, transfer)
            }
            _ => return None,
        };
        d.finish().ok()?;
        Some(record)
    }
}

/// Append [`Meta`] after a stage's own fields.
fn encode_meta(e: &mut Encoder, meta: &Meta) {
    e.fixed(&meta.started_unix_secs.to_be_bytes());
    match meta.ended_unix_secs {
        Some(ended) => {
            e.u8(1);
            e.fixed(&ended.to_be_bytes());
        }
        None => {
            e.u8(0);
        }
    }
    e.u8(match meta.direction {
        Direction::Pull => 0,
        Direction::Push => 1,
    });
    match &meta.batch_id {
        Some(id) => {
            e.u8(1);
            e.text(id);
        }
        None => {
            e.u8(0);
        }
    }
}

/// Read [`Meta`] back, or supply what an older record never wrote:
/// `fallback_started_unix_secs` for the start time, no end time, and
/// [`Direction::Pull`] for a version 1 record; no batch id for a version 1
/// or version 2 record.
fn decode_meta(d: &mut Decoder<'_>, version: u8, fallback_started_unix_secs: i64) -> Option<Meta> {
    if version == 1 {
        return Some(Meta {
            started_unix_secs: fallback_started_unix_secs,
            ended_unix_secs: None,
            direction: Direction::Pull,
            batch_id: None,
        });
    }
    let started_unix_secs = i64::from_be_bytes(d.fixed::<8>().ok()?);
    let ended_unix_secs = match d.u8().ok()? {
        0 => None,
        _ => Some(i64::from_be_bytes(d.fixed::<8>().ok()?)),
    };
    let direction = match d.u8().ok()? {
        0 => Direction::Pull,
        1 => Direction::Push,
        _ => return None,
    };
    // A version 2 record predates the batch id and carries no bytes for it.
    let batch_id = if version >= 3 {
        match d.u8().ok()? {
            0 => None,
            _ => Some(d.text(limits::MAX_PATH_LEN).ok()?.to_owned()),
        }
    } else {
        None
    };
    Some(Meta {
        started_unix_secs,
        ended_unix_secs,
        direction,
        batch_id,
    })
}

/// Read the record at `path`.
///
/// A missing file is not an error. It means the transfer has not started.
///
/// # Errors
///
/// Returns a `ManifestError::Wire` code when the file is there and cannot be
/// read as a record.
pub(crate) fn read_record(path: &Path) -> Result<Option<Record>, FerryError> {
    match fs::read(path) {
        Ok(bytes) => Record::decode(&bytes, fallback_started_unix_secs(path)).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(failed("TransferError::Local")),
    }
}

/// The record file's own modification time, in Unix seconds.
///
/// Used only for a version 1 record, which stored no start time of its own.
/// Falls back to now when the file's modification time cannot be read, since
/// a record that otherwise decodes correctly should not be refused over a
/// clock.
fn fallback_started_unix_secs(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_else(now_unix_secs)
}

/// Write the record at `path`, so a crash can never leave a short one.
///
/// # Errors
///
/// Returns a `TransferError::Local` code when local storage refuses the
/// write, and a `TransferError::NoRandomness` code when the temporary name
/// cannot be made.
pub(crate) fn write_record(path: &Path, record: &Record) -> Result<(), FerryError> {
    crate::privatefile::write_atomic(path, &record.encode())
}

#[cfg(test)]
mod tests {
    use super::{FirstPass, Meta, Record, read_record, write_record};
    use crate::Direction;
    use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
    use ferry_core::path::RemotePath;
    use ferry_core::session::Transfer;
    use ferry_core::wire::Encoder;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::UNIX_EPOCH;

    /// Every test gets its own folder, so tests in parallel never share one.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ferry-record-test-{}-{label}-{n}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("a folder for the test");
        dir
    }

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).expect("a valid path")
    }

    fn meta() -> Meta {
        Meta {
            started_unix_secs: 1_700_000_000,
            ended_unix_secs: None,
            direction: Direction::Pull,
            batch_id: None,
        }
    }

    fn meta_with_batch(batch_id: &str) -> Meta {
        Meta {
            batch_id: Some(batch_id.to_owned()),
            ..meta()
        }
    }

    fn first_pass() -> Record {
        Record::FirstPass(
            meta(),
            FirstPass {
                source: path("holiday.bin"),
                destination: path("photos/holiday.bin"),
                source_size: 8 * 1024 * 1024,
                source_mtime: -12,
                chunk_size: 1024 * 1024,
                chunks_done: 2,
            },
        )
    }

    fn ready() -> Record {
        let manifest = manifest_from_bytes(b"some bytes", ChunkSize::one_mebibyte());
        Record::Ready(
            meta(),
            Transfer::new(manifest, path("holiday.bin"), path("photos/holiday.bin"))
                .expect("a transfer identifier"),
        )
    }

    fn names_in(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("the folder should be readable")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_first_pass_record_survives_a_round_trip() {
        let dir = temp_dir("first-pass");
        let file = dir.join("one.bin");
        let record = first_pass();
        write_record(&file, &record).expect("the record should be written");
        let read = read_record(&file).expect("the record should be readable");
        assert_eq!(read, Some(record), "what was written comes back");
        assert_eq!(
            names_in(&dir),
            vec!["one.bin".to_owned()],
            "no temporary file is left behind"
        );
    }

    #[test]
    fn a_ready_record_survives_a_round_trip() {
        let dir = temp_dir("ready");
        let file = dir.join("two.bin");
        let record = ready();
        write_record(&file, &record).expect("the record should be written");
        let read = read_record(&file).expect("the record should be readable");
        assert_eq!(read, Some(record), "what was written comes back");
    }

    #[test]
    fn a_version_1_record_loads_with_the_files_mtime_no_end_and_pull() {
        // A hand-built version 1 record: the format before this batch added
        // a start time, an end time and a direction. This is the shape a
        // record left by an earlier build of Ferry is in.
        let dir = temp_dir("v1");
        let file = dir.join("five.bin");
        let mut e = Encoder::new();
        e.u8(1); // FORMAT_VERSION, before this batch
        e.u8(0); // STAGE_FIRST_PASS
        e.text("holiday.bin");
        e.text("photos/holiday.bin");
        e.u64(8 * 1024 * 1024);
        e.fixed(&(-12i64).to_be_bytes());
        e.u32(1024 * 1024);
        e.u32(2);
        fs::write(&file, e.finish()).expect("the hand-built record should write");

        let read = read_record(&file)
            .expect("a version 1 record should still load")
            .expect("the file is there");
        let Record::FirstPass(loaded_meta, pass) = read else {
            panic!("expected a first pass record");
        };
        assert_eq!(
            pass.source_size,
            8 * 1024 * 1024,
            "the fields version 1 always had still decode"
        );
        assert_eq!(
            loaded_meta.ended_unix_secs, None,
            "a version 1 record has no end time"
        );
        assert_eq!(
            loaded_meta.direction,
            Direction::Pull,
            "a version 1 record is always a pull"
        );

        let mtime = fs::metadata(&file)
            .expect("the file should have metadata")
            .modified()
            .expect("the file should carry a modified time")
            .duration_since(UNIX_EPOCH)
            .expect("the modified time is after the epoch")
            .as_secs();
        assert_eq!(
            loaded_meta.started_unix_secs,
            i64::try_from(mtime).expect("the mtime fits in an i64"),
            "its start time is the record file's modification time"
        );
    }

    #[test]
    fn a_records_batch_id_survives_a_round_trip() {
        let dir = temp_dir("batch-id");
        let file = dir.join("six.bin");
        let record = Record::FirstPass(
            meta_with_batch("device-abc"),
            FirstPass {
                source: path("holiday.bin"),
                destination: path("photos/holiday.bin"),
                source_size: 8 * 1024 * 1024,
                source_mtime: -12,
                chunk_size: 1024 * 1024,
                chunks_done: 2,
            },
        );
        write_record(&file, &record).expect("the record should be written");
        let read = read_record(&file)
            .expect("the record should be readable")
            .expect("the file is there");
        let Record::FirstPass(loaded_meta, _) = read else {
            panic!("expected a first pass record");
        };
        assert_eq!(
            loaded_meta.batch_id,
            Some("device-abc".to_owned()),
            "the batch id comes back with the record"
        );
    }

    #[test]
    fn a_version_2_record_loads_with_no_batch_id() {
        // A hand-built version 2 record: the format before this batch added
        // a batch id. This is the shape a record batch B's build leaves.
        let dir = temp_dir("v2");
        let file = dir.join("seven.bin");
        let mut e = Encoder::new();
        e.u8(2); // FORMAT_VERSION, before this batch
        e.u8(0); // STAGE_FIRST_PASS
        e.text("holiday.bin");
        e.text("photos/holiday.bin");
        e.u64(8 * 1024 * 1024);
        e.fixed(&(-12i64).to_be_bytes());
        e.u32(1024 * 1024);
        e.u32(2);
        // Meta, as version 2 wrote it: start, end flag, direction. No batch
        // id bytes follow.
        e.fixed(&1_700_000_000i64.to_be_bytes());
        e.u8(0); // no end time
        e.u8(0); // Direction::Pull
        fs::write(&file, e.finish()).expect("the hand-built record should write");

        let read = read_record(&file)
            .expect("a version 2 record should still load")
            .expect("the file is there");
        let Record::FirstPass(loaded_meta, _) = read else {
            panic!("expected a first pass record");
        };
        assert_eq!(
            loaded_meta.batch_id, None,
            "a version 2 record predates the batch id"
        );
        assert_eq!(loaded_meta.started_unix_secs, 1_700_000_000);
    }

    #[test]
    fn a_missing_record_is_not_an_error() {
        let dir = temp_dir("missing");
        let read = read_record(&dir.join("nothing.bin")).expect("a missing file is no error");
        assert_eq!(read, None, "a transfer with no record starts a first pass");
    }

    #[test]
    fn a_short_record_is_refused_rather_than_half_read() {
        let dir = temp_dir("short");
        let file = dir.join("three.bin");
        let bytes = first_pass().encode();
        fs::write(&file, &bytes[..bytes.len() / 2]).expect("the test may write a short file");
        let read = read_record(&file);
        assert!(read.is_err(), "half a record is not a record");
    }

    #[test]
    fn a_failed_write_leaves_the_record_that_was_there() {
        let dir = temp_dir("failed");
        let file = dir.join("four.bin");
        write_record(&file, &first_pass()).expect("the first write should work");

        // A folder that refuses new files is the closest a test can get to a
        // machine that dies in the middle of the write.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = fs::metadata(&dir)
                .expect("the folder is there")
                .permissions();
            mode.set_mode(0o500);
            fs::set_permissions(&dir, mode).expect("the mode should be set");
            let second = write_record(&file, &ready());
            let mut mode = fs::metadata(&dir)
                .expect("the folder is there")
                .permissions();
            mode.set_mode(0o700);
            fs::set_permissions(&dir, mode).expect("the mode should be set back");
            assert!(second.is_err(), "a write that cannot happen is an error");
        }

        let read = read_record(&file).expect("the old record is still readable");
        assert_eq!(read, Some(first_pass()), "and it is the whole old record");
    }

    #[test]
    fn transfers_are_written_through_this_module_and_no_other_way() {
        // This is the structural half of the fix. `std::fs::write` is what
        // left a short record behind, so no part of a transfer may call it.
        let source = include_str!("transfer.rs");
        assert!(
            !source.contains("fs::write("),
            "a transfer record must not be written with std::fs::write"
        );
        assert!(
            source.contains("write_record("),
            "a transfer record is written through write_record"
        );
    }
}
