//! The held index: every file this device has pulled to completion.
//!
//! `docs/engine-contract.md`, item 14. One row per completed pull, manual or
//! automatic, is what lets the run in `auto_copy.rs` know a file already
//! made it across, even once it has been renamed on the peer or deleted from
//! the download folder. Job 7 says "never copied twice", so a row here is
//! never removed by anything in this crate: not a later pull, not the person
//! deleting the file afterwards, not a device being forgotten.
//!
//! Stored the way `ferry-core`'s `PeerStore` stores the paired device list:
//! a version byte, a bounded count, and the bytes go to a temporary name and
//! are renamed over the real one, so a crash mid write never leaves a short
//! file behind. The pattern is written again here rather than shared, the
//! same choice `record.rs` documents for its own copy of it.
//!
//! Unlike the paired device list, a corrupt or missing index is not an
//! engine startup failure. It is a cache of what this device already holds,
//! not something the engine cannot run without: the worst a lost row costs
//! is one file copied again.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ferry_core::limits;
use ferry_core::session::SessionId;
use ferry_core::wire::{Decoder, Encoder};

use crate::FerryError;
use crate::errors::failed;

/// The newest format this build writes, and the only one it reads.
const FORMAT_VERSION: u8 = 1;

/// The most rows the index keeps. Past this, the oldest row is dropped. This
/// is why the rows live in a `VecDeque`, oldest first, rather than a set:
/// eviction has to know which row arrived first.
const MAX_ROWS: usize = 100_000;

/// The most bytes a device key, in hex, may take. A public key is always 64
/// hex characters, so this only ever bounds a corrupt or hostile file.
const MAX_DEVICE_KEY_LEN: usize = 64;

/// One file this device has pulled to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldRow {
    /// The device the file came from, as 64 lowercase hex characters.
    pub(crate) device_key_hex: String,
    /// Where the file lived on that device, root-relative.
    pub(crate) source_path: String,
    /// The file's size at the moment the pull that wrote this row finished.
    pub(crate) size: u64,
    /// The file's modified time at that same moment.
    pub(crate) mtime: i64,
    /// The whole file's BLAKE3 root hash, from the transfer's own manifest.
    pub(crate) root: [u8; 32],
}

/// Every file this device has pulled to completion, persisted to one file.
#[derive(Debug, Clone)]
pub(crate) struct HeldStore {
    path: PathBuf,
    /// Oldest first, so [`HeldStore::record`] knows which row to drop once
    /// [`MAX_ROWS`] is passed.
    rows: VecDeque<HeldRow>,
}

impl HeldStore {
    /// Load the index from `path`.
    ///
    /// A missing file, or one that fails to decode, comes back as an empty
    /// index. See the module documentation for why that is not an error.
    pub(crate) fn load(path: &Path) -> Self {
        let rows = fs::read(path)
            .ok()
            .and_then(|bytes| decode_rows(&bytes))
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            rows,
        }
    }

    /// Write the index out, replacing whatever was there before.
    ///
    /// # Errors
    ///
    /// Returns `TransferError::Local` when local storage refuses the write,
    /// and `TransferError::NoRandomness` when the temporary name cannot be
    /// made.
    pub(crate) fn save(&self) -> Result<(), FerryError> {
        write_private_file(&self.path, &encode_rows(&self.rows))
    }

    /// Add a row, dropping the oldest once the index is over [`MAX_ROWS`].
    pub(crate) fn record(&mut self, row: HeldRow) {
        self.rows.push_back(row);
        while self.rows.len() > MAX_ROWS {
            self.rows.pop_front();
        }
    }

    /// True when a row names this exact device, path, size and modified
    /// time. The cheap check the run makes before it asks the peer for
    /// anything.
    pub(crate) fn contains_path(
        &self,
        device_key_hex: &str,
        path: &str,
        size: u64,
        mtime: i64,
    ) -> bool {
        self.rows.iter().any(|row| {
            row.device_key_hex == device_key_hex
                && row.source_path == path
                && row.size == size
                && row.mtime == mtime
        })
    }

    /// True when a row, from any device or path, carries this root hash.
    /// What the run checks once it has fetched a candidate's manifest.
    pub(crate) fn contains_root(&self, root: &[u8; 32]) -> bool {
        self.rows.iter().any(|row| &row.root == root)
    }

    /// How many rows the index holds. Test only.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
}

// Read the stored rows. A wrong version byte or an over-large count is
// caught before anything else is read, so a corrupted or hostile file
// cannot make this allocate more than the limit allows.
fn decode_rows(bytes: &[u8]) -> Option<VecDeque<HeldRow>> {
    let mut d = Decoder::new(bytes);
    let version = d.u8().ok()?;
    if version != FORMAT_VERSION {
        return None;
    }
    let count = d.u32().ok()?;
    if count as usize > MAX_ROWS {
        return None;
    }
    let mut rows = VecDeque::with_capacity(usize::try_from(count).ok()?);
    for _ in 0..count {
        let device_key_hex = d.text(MAX_DEVICE_KEY_LEN).ok()?.to_owned();
        let source_path = d.text(limits::MAX_PATH_LEN).ok()?.to_owned();
        let size = d.u64().ok()?;
        let mtime = decode_i64(d.u64().ok()?);
        let root = d.fixed::<32>().ok()?;
        rows.push_back(HeldRow {
            device_key_hex,
            source_path,
            size,
            mtime,
            root,
        });
    }
    d.finish().ok()?;
    Some(rows)
}

fn encode_rows(rows: &VecDeque<HeldRow>) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(FORMAT_VERSION);
    let count = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    e.u32(count);
    for row in rows {
        e.text(&row.device_key_hex);
        e.text(&row.source_path);
        e.u64(row.size);
        e.u64(encode_i64(row.mtime));
        e.fixed(&row.root);
    }
    e.finish()
}

// `Encoder` and `Decoder` have no signed integer methods, so a modified time
// is carried as its bit pattern instead. `ferry-core`'s `peers.rs` does the
// same, for the same reason.
fn encode_i64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

fn decode_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

// Write `bytes` to `path` so a crash mid write can never leave a short file
// at `path`. The pattern `record.rs` and `batch.rs` each write again for
// their own file, rather than sharing one function for all three.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), FerryError> {
    let temporary = temporary_name(path)?;
    let written = write_and_sync(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        drop(fs::remove_file(&temporary));
        return Err(failed("TransferError::Local"));
    }
    Ok(())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn temporary_name(path: &Path) -> Result<PathBuf, FerryError> {
    let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{session}.tmp"));
    Ok(PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::{HeldRow, HeldStore, MAX_ROWS};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ferry-held-test-{}-{label}-{n}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("a folder for the test");
        dir
    }

    fn row(device: &str, path: &str, size: u64, mtime: i64, root_byte: u8) -> HeldRow {
        HeldRow {
            device_key_hex: device.to_owned(),
            source_path: path.to_owned(),
            size,
            mtime,
            root: [root_byte; 32],
        }
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_index() {
        let dir = temp_dir("missing");
        let store = HeldStore::load(&dir.join("held"));
        assert_eq!(store.len(), 0);
        assert!(!store.contains_path("device", "a.bin", 1, 0));
    }

    #[test]
    fn record_then_save_then_load_round_trips() {
        let dir = temp_dir("round-trip");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        store.record(row("device-1", "DCIM/a.jpg", 1024, -5, 1));
        store.record(row("device-2", "DCIM/b.jpg", 2048, 500, 2));
        store.save().expect("the index should save");

        let loaded = HeldStore::load(&path);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains_path("device-1", "DCIM/a.jpg", 1024, -5));
        assert!(loaded.contains_root(&[2; 32]));
        assert!(!loaded.contains_root(&[9; 32]));
    }

    #[test]
    fn contains_path_needs_every_field_to_match() {
        let mut store = HeldStore::load(&temp_dir("path-match").join("held"));
        store.record(row("device", "DCIM/a.jpg", 1024, 500, 1));
        assert!(store.contains_path("device", "DCIM/a.jpg", 1024, 500));
        assert!(!store.contains_path("other", "DCIM/a.jpg", 1024, 500));
        assert!(!store.contains_path("device", "DCIM/b.jpg", 1024, 500));
        assert!(!store.contains_path("device", "DCIM/a.jpg", 2048, 500));
        assert!(!store.contains_path("device", "DCIM/a.jpg", 1024, 501));
    }

    #[test]
    fn contains_root_matches_across_devices_and_paths() {
        let mut store = HeldStore::load(&temp_dir("root-match").join("held"));
        store.record(row("device-1", "DCIM/a.jpg", 1024, 0, 7));
        assert!(
            store.contains_root(&[7; 32]),
            "the same content pulled under a different device or path still counts as held"
        );
    }

    #[test]
    fn past_the_bound_the_oldest_row_is_dropped() {
        let mut store = HeldStore::load(&temp_dir("bound").join("held"));
        for i in 0..MAX_ROWS {
            let byte = u8::try_from(i % 256).unwrap_or(0);
            store.record(row("device", &format!("DCIM/{i}.jpg"), 1, 0, byte));
        }
        assert_eq!(store.len(), MAX_ROWS);
        assert!(store.contains_path("device", "DCIM/0.jpg", 1, 0));

        store.record(row("device", "DCIM/new.jpg", 1, 0, 255));
        assert_eq!(store.len(), MAX_ROWS, "the bound is never crossed");
        assert!(
            !store.contains_path("device", "DCIM/0.jpg", 1, 0),
            "the oldest row is the one dropped"
        );
        assert!(store.contains_path("device", "DCIM/new.jpg", 1, 0));
    }

    #[test]
    fn a_tampered_format_version_is_refused_and_loads_empty() {
        let dir = temp_dir("bad-version");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        store.record(row("device", "DCIM/a.jpg", 1, 0, 1));
        store.save().expect("the index should save");

        let mut bytes = fs::read(&path).expect("the file should be there");
        bytes[0] = 99;
        fs::write(&path, &bytes).expect("the tampered bytes should write");

        let loaded = HeldStore::load(&path);
        assert_eq!(
            loaded.len(),
            0,
            "a file this build cannot read loads as empty, not a startup failure"
        );
    }
}
