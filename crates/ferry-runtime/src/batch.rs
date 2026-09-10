//! The batch record on disk: one file per batch, holding what does not
//! change once the batch is made.
//!
//! `docs/engine-contract.md`, batch D, item 2: aggregates — files done,
//! bytes, state, speed, and ended — are computed live from the transfer
//! rows, never trusted alone. This module stores the rest: the label, the
//! origin, the direction, when the batch started, and which transfers
//! belong to it, in the order they were queued. The device key is not
//! stored either: like a transfer id, a batch id carries it as the text
//! before the first `-`, and the loader in `engine.rs` reads it from there.
//!
//! It also stores a floor for `files_done` and `bytes_done`: a `Done`
//! transfer's own record is deleted the moment it finishes, so a restart
//! only ever reloads the transfers still in progress, and the live count
//! alone would show fewer done than really are, or none at all, for every
//! batch that outlives a restart. `transfer::finish` raises this floor by
//! one transfer's `bytes_total` each time a transfer in the batch reaches
//! `Done`, and writes the record down. `BatchRow::info` reports whichever of
//! the floor and the live count is larger.
//!
//! Written the way `record.rs` writes a transfer record: a version byte,
//! then the bytes go to a temporary name and are renamed over the real one,
//! so a crash mid write never leaves a short file behind. The pattern is
//! written again here rather than shared, the same choice `record.rs`
//! documents for `peers.rs`'s own copy of it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ferry_core::limits;
use ferry_core::session::SessionId;
use ferry_core::wire::{Decoder, Encoder};

use crate::errors::failed;
use crate::state::BatchRow;
use crate::{Direction, FerryError, Origin};

/// The version byte every batch record starts with.
///
/// Version 1 held no `done_files` or `done_bytes`. A version 1 file still
/// loads: both come back zero, which understates a batch that already had
/// some files done under that older build, but never overstates one, and
/// the live count still fills in whatever the version 1 file could not
/// have known to raise. See [`BatchRecord::decode`].
const FORMAT_VERSION: u8 = 2;

/// The format version before `done_files` and `done_bytes` were added.
const FORMAT_VERSION_1: u8 = 1;

/// What one batch record holds. The device key and the batch id are not
/// part of this: both live in the record's file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchRecord {
    /// The remote path as given to `pull_folder`.
    pub(crate) label: String,
    /// Why this batch exists.
    pub(crate) origin: Origin,
    /// Which way every transfer in this batch moves its file.
    pub(crate) direction: Direction,
    /// When `pull_folder` created this batch.
    pub(crate) started_unix_secs: i64,
    /// The transfer ids this batch covers, in the order they were queued.
    pub(crate) transfer_ids: Vec<String>,
    /// How many of `transfer_ids` had reached `Done` as of the last write.
    /// A floor, not a live count: see the module documentation.
    pub(crate) done_files: u32,
    /// The sum of `bytes_total` over the transfers `done_files` counts.
    pub(crate) done_bytes: u64,
}

impl BatchRecord {
    /// Build the record a [`BatchRow`] should be stored as. The id and the
    /// device key stay out of it; see the module documentation.
    pub(crate) fn of(row: &BatchRow) -> Self {
        Self {
            label: row.label.clone(),
            origin: row.origin,
            direction: row.direction,
            started_unix_secs: row.started_unix_secs,
            transfer_ids: row.transfer_ids.clone(),
            done_files: row.done_files,
            done_bytes: row.done_bytes,
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION);
        e.text(&self.label);
        e.u8(match self.origin {
            Origin::Manual => 0,
            Origin::Automatic => 1,
        });
        e.u8(match self.direction {
            Direction::Pull => 0,
            Direction::Push => 1,
        });
        e.fixed(&self.started_unix_secs.to_be_bytes());
        let count = u32::try_from(self.transfer_ids.len()).unwrap_or(u32::MAX);
        e.u32(count);
        for id in &self.transfer_ids {
            e.text(id);
        }
        e.u32(self.done_files);
        e.u64(self.done_bytes);
        e.finish()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut d = Decoder::new(bytes);
        let version = d.u8().ok()?;
        if version != FORMAT_VERSION && version != FORMAT_VERSION_1 {
            return None;
        }
        let label = d.text(limits::MAX_PATH_LEN).ok()?.to_owned();
        let origin = match d.u8().ok()? {
            0 => Origin::Manual,
            1 => Origin::Automatic,
            _ => return None,
        };
        let direction = match d.u8().ok()? {
            0 => Direction::Pull,
            1 => Direction::Push,
            _ => return None,
        };
        let started_unix_secs = i64::from_be_bytes(d.fixed::<8>().ok()?);
        let count = d.u32().ok()?;
        // Refused before `Vec::with_capacity` reserves anything: `pull_folder`
        // never queues more than `MAX_FOLDER_FILES` transfers into one batch,
        // so a count above that is not a batch this crate ever wrote, and
        // reserving space for it first would let a bad file's count alone
        // decide how much memory this load takes.
        if count as usize > crate::folder::MAX_FOLDER_FILES {
            return None;
        }
        let mut transfer_ids = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for _ in 0..count {
            transfer_ids.push(d.text(limits::MAX_PATH_LEN).ok()?.to_owned());
        }
        let (done_files, done_bytes) = if version == FORMAT_VERSION_1 {
            (0, 0)
        } else {
            (d.u32().ok()?, d.u64().ok()?)
        };
        d.finish().ok()?;
        Some(Self {
            label,
            origin,
            direction,
            started_unix_secs,
            transfer_ids,
            done_files,
            done_bytes,
        })
    }
}

/// Read the batch record at `path`.
///
/// A missing file, or one that does not decode, comes back `None`. This
/// mirrors `load_saved_transfers` in `engine.rs`, which skips a transfer
/// record the same way: a batch record only matters at start, this crate
/// never writes one that fails to decode, and dropping a damaged one is
/// safer than guessing at it.
pub(crate) fn read_batch(path: &Path) -> Option<BatchRecord> {
    let bytes = fs::read(path).ok()?;
    BatchRecord::decode(&bytes)
}

/// Write the batch record at `path`, the temp-then-rename way.
///
/// # Errors
///
/// Returns `TransferError::Local` when local storage refuses the write, and
/// `TransferError::NoRandomness` when the temporary name cannot be made.
pub(crate) fn write_batch(path: &Path, record: &BatchRecord) -> Result<(), FerryError> {
    let bytes = record.encode();
    let temporary = temporary_name(path)?;
    let written = write_and_sync(&temporary, &bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        // A temporary file left behind is never read again, but it would
        // otherwise sit in the app's own folder for ever.
        drop(fs::remove_file(&temporary));
        return Err(failed("TransferError::Local"));
    }
    Ok(())
}

/// Create the file, write every byte through that one handle, and flush it
/// to the disk. `create_new` refuses to write through anything already at
/// the name, including a symbolic link someone planted there.
fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// A name next to `path` that nothing can be waiting at.
fn temporary_name(path: &Path) -> Result<PathBuf, FerryError> {
    let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{session}.tmp"));
    Ok(PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::{BatchRecord, read_batch, write_batch};
    use crate::{Direction, Origin};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ferry-batch-test-{}-{label}-{n}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("a folder for the test");
        dir
    }

    fn sample() -> BatchRecord {
        BatchRecord {
            label: "Internal storage/DCIM/Camera".to_owned(),
            origin: Origin::Manual,
            direction: Direction::Pull,
            started_unix_secs: 1_700_000_000,
            transfer_ids: vec!["a-1".to_owned(), "a-2".to_owned()],
            done_files: 1,
            done_bytes: 4_096,
        }
    }

    #[test]
    fn a_batch_record_survives_a_round_trip() {
        let dir = temp_dir("round-trip");
        let file = dir.join("batch-1");
        let record = sample();
        write_batch(&file, &record).expect("the record should be written");
        let read = read_batch(&file).expect("the record should be readable");
        assert_eq!(read, record, "what was written comes back");
    }

    #[test]
    fn an_empty_transfer_list_round_trips() {
        let dir = temp_dir("empty");
        let file = dir.join("batch-2");
        let record = BatchRecord {
            transfer_ids: Vec::new(),
            ..sample()
        };
        write_batch(&file, &record).expect("the record should be written");
        let read = read_batch(&file).expect("the record should be readable");
        assert_eq!(read.transfer_ids, Vec::<String>::new());
    }

    #[test]
    fn a_missing_file_reads_as_none() {
        let dir = temp_dir("missing");
        assert!(read_batch(&dir.join("nothing")).is_none());
    }

    #[test]
    fn a_short_file_reads_as_none_rather_than_half_read() {
        let dir = temp_dir("short");
        let file = dir.join("batch-3");
        let bytes = sample().encode();
        fs::write(&file, &bytes[..bytes.len() / 2]).expect("the test may write a short file");
        assert!(read_batch(&file).is_none());
    }

    #[test]
    fn a_huge_transfer_count_is_refused_before_it_is_reserved() {
        use ferry_core::wire::Encoder;

        let dir = temp_dir("huge-count");
        let file = dir.join("batch-5");
        let mut e = Encoder::new();
        e.u8(2); // FORMAT_VERSION
        e.text("Internal storage/DCIM/Camera");
        e.u8(0); // Origin::Manual
        e.u8(0); // Direction::Pull
        e.fixed(&1_700_000_000i64.to_be_bytes());
        // Far more than `pull_folder` ever queues into one batch, and with
        // none of the ids this count promises actually following. A build
        // that reserved space for this count first, before checking it,
        // would try to allocate room for billions of ids.
        e.u32(u32::MAX);
        fs::write(&file, e.finish()).expect("the hand-built file should write");

        assert!(
            read_batch(&file).is_none(),
            "a count this large is refused outright, not allocated for"
        );
    }

    #[test]
    fn a_version_1_file_loads_with_no_done_count() {
        use ferry_core::wire::Encoder;

        let dir = temp_dir("version-1");
        let file = dir.join("batch-4");
        let mut e = Encoder::new();
        e.u8(1); // FORMAT_VERSION_1
        e.text("Internal storage/DCIM/Camera");
        e.u8(0); // Origin::Manual
        e.u8(0); // Direction::Pull
        e.fixed(&1_700_000_000i64.to_be_bytes());
        e.u32(2);
        e.text("a-1");
        e.text("a-2");
        fs::write(&file, e.finish()).expect("the hand-built version 1 file should write");

        let read = read_batch(&file).expect("a version 1 batch file should still load");
        assert_eq!(read.transfer_ids, vec!["a-1".to_owned(), "a-2".to_owned()]);
        assert_eq!(
            (read.done_files, read.done_bytes),
            (0, 0),
            "a version 1 file predates the done count; it loads as zero, not a decode failure"
        );
    }
}
