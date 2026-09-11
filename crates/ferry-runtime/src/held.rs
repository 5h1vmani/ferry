//! The held index: every file this device has pulled to completion.
//!
//! `docs/engine-contract.md`, item 14. One row per completed pull, manual or
//! automatic, is what lets the run in `auto_copy.rs` know a file already
//! made it across, even once it has been renamed on the peer or deleted from
//! the download folder. Job 7 says "never copied twice", so a row here is
//! never removed by anything in this crate: not a later pull, not the person
//! deleting the file afterwards, not a device being forgotten.
//!
//! Stored the way `access.rs` stores one day's file: a version byte,
//! written once, then each row appended as its own length-prefixed frame.
//! [`HeldStore::record`] appends one frame per completed pull rather than
//! rewriting and syncing the whole file, so a busy pull batch costs one
//! small write per file, not one full rewrite each. A frame cut short by a
//! crash mid append is dropped the next time the file is loaded, the same
//! torn tail handling `access.rs` gives a day file.
//!
//! Past [`MAX_ROWS`] rows, `record` rebuilds the file compact: the oldest
//! rows are dropped and what remains is written out fresh, to a temporary
//! name that is then renamed over the real one, the pattern `record.rs` and
//! `ferry-core`'s `PeerStore` use for their own whole-file rewrites. This
//! only happens once every `MAX_ROWS` rows, not on every completed pull.
//!
//! Unlike the paired device list, a corrupt or missing index is not an
//! engine startup failure. It is a cache of what this device already holds,
//! not something the engine cannot run without: the worst a lost row costs
//! is one file copied again.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ferry_core::limits;
use ferry_core::session::SessionId;
use ferry_core::wire::{Decoder, Encoder};

use crate::FerryError;
use crate::errors::failed;

/// The newest format this build writes, and the only one it reads.
///
/// Bumped from the version this file used before G1: that format held one
/// count-prefixed blob of every row, rewritten whole on every save. This
/// format holds a version byte followed by independently framed rows, an
/// incompatible shape, so an old file is refused by [`HeldStore::load`]
/// exactly like any other format this build does not know, rather than
/// misread as the new shape.
const FORMAT_VERSION: u8 = 2;

/// The most rows the index keeps. Past this, [`HeldStore::record`] rebuilds
/// the file with the oldest rows dropped. This is why the rows live in a
/// `VecDeque`, oldest first, rather than a set: eviction has to know which
/// row arrived first.
const MAX_ROWS: usize = 100_000;

/// The most bytes a device key, in hex, may take. A public key is always 64
/// hex characters, so this only ever bounds a corrupt or hostile file.
const MAX_DEVICE_KEY_LEN: usize = 64;

/// A generous bound on one row's encoded frame: well over 64 bytes of
/// device key hex, [`limits::MAX_PATH_LEN`] bytes of path, a size, a
/// modified time, a root hash, and the two length prefixes those take. Only
/// bounds how much a corrupt or hostile file can make one read allocate,
/// the same role `access.rs`'s `MAX_ENTRY_FRAME_BYTES` plays there.
const MAX_ROW_FRAME_BYTES: usize = 2048;

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

/// The `(device, path, size, mtime)` a [`HeldRow`] is looked up and
/// deduplicated by. A tuple, not a fourth kind of struct, since nothing
/// outside this module ever names one.
type PathKey = (String, String, u64, i64);

fn path_key_of(row: &HeldRow) -> PathKey {
    (
        row.device_key_hex.clone(),
        row.source_path.clone(),
        row.size,
        row.mtime,
    )
}

/// Every file this device has pulled to completion, persisted to one file.
#[derive(Debug, Clone)]
pub(crate) struct HeldStore {
    path: PathBuf,
    /// Oldest first, so [`HeldStore::record`] knows which row to drop once
    /// [`MAX_ROWS`] is passed.
    rows: VecDeque<HeldRow>,
    /// G3: every row's [`PathKey`], kept in lockstep with `rows`, so
    /// [`HeldStore::contains_path`] is a lookup instead of a scan. A
    /// `record` skips a row whose key is already here, so a key never
    /// names more than one row at a time, and dropping the oldest row
    /// always removes exactly the key that row added.
    path_keys: HashSet<PathKey>,
    /// G3: how many rows currently hold each root hash, kept in lockstep
    /// with `rows`, so [`HeldStore::contains_root`] is a lookup instead of
    /// a scan. A count, not a plain set, because the same content pulled
    /// under a different device or path is a second row with the same
    /// root hash, and dropping one of those rows must not make the other's
    /// root hash disappear from the set.
    roots: HashMap<[u8; 32], usize>,
    /// How many times [`HeldStore::record`] has rebuilt the whole file.
    /// Test only: proves a batch of completed pulls appends instead of
    /// rewriting.
    #[cfg(test)]
    rewrites: usize,
}

impl HeldStore {
    /// Load the index from `path`.
    ///
    /// A missing file, or one whose version this build does not know,
    /// comes back as an empty index. See the module documentation for why
    /// that is not an error. A file in the right format but cut short by a
    /// crash mid append is repaired in place: cut back to its last whole
    /// row, so the next [`HeldStore::record`] appends right after it
    /// instead of after an unreadable frame.
    pub(crate) fn load(path: &Path) -> Self {
        let rows = match fs::read(path) {
            Ok(bytes) if bytes.first() == Some(&FORMAT_VERSION) => {
                let (rows, good_len) = decode_rows(&bytes);
                if good_len < bytes.len() {
                    repair_torn_tail(path, good_len);
                }
                rows
            }
            _ => VecDeque::new(),
        };
        let mut store = Self {
            path: path.to_path_buf(),
            rows: VecDeque::new(),
            path_keys: HashSet::new(),
            roots: HashMap::new(),
            #[cfg(test)]
            rewrites: 0,
        };
        for row in rows {
            store.index_row(&row);
            store.rows.push_back(row);
        }
        store
    }

    /// Add `row`'s key and root hash to the indexes, without touching
    /// `rows` itself. Every caller that adds a row to `rows` calls this
    /// first, so the two never drift apart.
    fn index_row(&mut self, row: &HeldRow) {
        self.path_keys.insert(path_key_of(row));
        *self.roots.entry(row.root).or_insert(0) += 1;
    }

    /// Remove `row`'s key and root hash from the indexes, the inverse of
    /// [`HeldStore::index_row`]. Called only for the row [`HeldStore::
    /// record`] drops once [`MAX_ROWS`] is passed.
    fn unindex_row(&mut self, row: &HeldRow) {
        self.path_keys.remove(&path_key_of(row));
        if let Some(count) = self.roots.get_mut(&row.root) {
            *count -= 1;
            if *count == 0 {
                self.roots.remove(&row.root);
            }
        }
    }

    /// Rebuild the file to hold exactly the rows this store has in memory,
    /// replacing whatever was there before. Used to rebuild it compact once
    /// [`MAX_ROWS`] is passed; also used directly by tests.
    ///
    /// # Errors
    ///
    /// Returns `TransferError::Local` when local storage refuses the write,
    /// and `TransferError::NoRandomness` when the temporary name cannot be
    /// made.
    pub(crate) fn save(&mut self) -> Result<(), FerryError> {
        #[cfg(test)]
        {
            self.rewrites += 1;
        }
        write_private_file(&self.path, &encode_rows(&self.rows))
    }

    /// Add a row, appending it to the file as one framed write rather than
    /// rewriting the whole thing. Once the index passes [`MAX_ROWS`] rows,
    /// the file is rebuilt compact instead, with the oldest rows dropped;
    /// that is the only time this rewrites rather than appends.
    ///
    /// G3: a row whose device, path, size and modified time already name a
    /// row here is an exact duplicate (the same file, recorded held twice)
    /// and is skipped: it would only waste a slot `MAX_ROWS` could give a
    /// genuinely different file.
    ///
    /// Best effort: a failed write here, like a failed [`HeldStore::save`],
    /// costs one file copied again, never a wrong answer. See the module
    /// documentation.
    ///
    /// # Errors
    ///
    /// Returns `TransferError::Local` when local storage refuses the
    /// append or the rebuild.
    pub(crate) fn record(&mut self, row: HeldRow) -> Result<(), FerryError> {
        if self.path_keys.contains(&path_key_of(&row)) {
            return Ok(());
        }
        self.index_row(&row);
        self.rows.push_back(row);
        if self.rows.len() > MAX_ROWS {
            // `pop_front` never returns `None` here: the length just
            // checked above is at least one past `MAX_ROWS`, which is
            // itself well above zero.
            if let Some(dropped) = self.rows.pop_front() {
                self.unindex_row(&dropped);
            }
            return self.save();
        }
        // `push_back` above always leaves the new row last.
        let row = self.rows.back().expect("a row was just pushed");
        append_row(&self.path, row)
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
        self.path_keys
            .contains(&(device_key_hex.to_owned(), path.to_owned(), size, mtime))
    }

    /// True when a row, from any device or path, carries this root hash.
    /// What the run checks once it has fetched a candidate's manifest.
    pub(crate) fn contains_root(&self, root: &[u8; 32]) -> bool {
        self.roots.contains_key(root)
    }

    /// How many rows the index holds. Test only.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
}

// Decode every whole row in a file's bytes, alongside how many leading
// bytes those whole rows take up. `bytes` is assumed to already start with
// [`FORMAT_VERSION`]; `HeldStore::load` checks that before calling this.
//
// A frame whose declared length runs past the end of `bytes`, or whose
// content does not decode cleanly, stops the read where it is rather than
// failing the whole load: this file is only ever appended to or rebuilt
// whole, so that can only be a crash mid append. Every row before it is
// still whole and is still returned. Mirrors `access.rs`'s
// `decode_day_file`.
fn decode_rows(bytes: &[u8]) -> (VecDeque<HeldRow>, usize) {
    let mut d = Decoder::new(&bytes[1..]);
    let mut rows = VecDeque::new();
    let mut good_len = 1; // the version byte
    loop {
        let before = d.remaining();
        if before == 0 {
            break;
        }
        let Ok(frame) = d.bytes(MAX_ROW_FRAME_BYTES) else {
            break;
        };
        let Some(row) = decode_row(frame) else {
            break;
        };
        rows.push_back(row);
        good_len += before - d.remaining();
    }
    (rows, good_len)
}

fn decode_row(bytes: &[u8]) -> Option<HeldRow> {
    let mut d = Decoder::new(bytes);
    let device_key_hex = d.text(MAX_DEVICE_KEY_LEN).ok()?.to_owned();
    let source_path = d.text(limits::MAX_PATH_LEN).ok()?.to_owned();
    let size = d.u64().ok()?;
    let mtime = decode_i64(d.u64().ok()?);
    let root = d.fixed::<32>().ok()?;
    d.finish().ok()?;
    Some(HeldRow {
        device_key_hex,
        source_path,
        size,
        mtime,
        root,
    })
}

fn encode_row(row: &HeldRow) -> Vec<u8> {
    let mut e = Encoder::new();
    e.text(&row.device_key_hex);
    e.text(&row.source_path);
    e.u64(row.size);
    e.u64(encode_i64(row.mtime));
    e.fixed(&row.root);
    e.finish()
}

fn encode_rows(rows: &VecDeque<HeldRow>) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(FORMAT_VERSION);
    for row in rows {
        e.bytes(&encode_row(row));
    }
    e.finish()
}

// Append one row to `path` as a single framed write: a four byte big
// endian length, then `encode_row`'s bytes, with the version byte written
// first only if the file is new or empty. Mirrors `access.rs`'s
// `AccessLog::append`.
fn append_row(path: &Path, row: &HeldRow) -> Result<(), FerryError> {
    let needs_header = fs::metadata(path)
        .map(|meta| meta.len() == 0)
        .unwrap_or(true);
    let mut frame = Encoder::new();
    if needs_header {
        frame.u8(FORMAT_VERSION);
    }
    frame.bytes(&encode_row(row));
    (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        file.write_all(&frame.finish())
    })()
    .map_err(|_| failed("TransferError::Local"))
}

// Cut `path` back to its first `good_len` bytes. Called only when `load`
// finds a row's frame that runs past the end of the file: since this file
// is only ever appended to or rebuilt whole by `HeldStore` itself, that can
// only be a single unfinished frame left by a crash mid append, and cutting
// it away is always safe. A failure to open or truncate is not surfaced:
// the in-memory rows already reflect the good prefix, so the worst this
// costs is one more torn tail found on the next load.
fn repair_torn_tail(path: &Path, good_len: usize) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        drop(file.set_len(u64::try_from(good_len).unwrap_or(0)));
    }
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
    fn record_then_load_round_trips() {
        let dir = temp_dir("round-trip");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        store
            .record(row("device-1", "DCIM/a.jpg", 1024, -5, 1))
            .expect("the record should append");
        store
            .record(row("device-2", "DCIM/b.jpg", 2048, 500, 2))
            .expect("the record should append");

        let loaded = HeldStore::load(&path);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains_path("device-1", "DCIM/a.jpg", 1024, -5));
        assert!(loaded.contains_root(&[2; 32]));
        assert!(!loaded.contains_root(&[9; 32]));
    }

    #[test]
    fn contains_path_needs_every_field_to_match() {
        let mut store = HeldStore::load(&temp_dir("path-match").join("held"));
        store
            .record(row("device", "DCIM/a.jpg", 1024, 500, 1))
            .expect("the record should append");
        assert!(store.contains_path("device", "DCIM/a.jpg", 1024, 500));
        assert!(!store.contains_path("other", "DCIM/a.jpg", 1024, 500));
        assert!(!store.contains_path("device", "DCIM/b.jpg", 1024, 500));
        assert!(!store.contains_path("device", "DCIM/a.jpg", 2048, 500));
        assert!(!store.contains_path("device", "DCIM/a.jpg", 1024, 501));
    }

    #[test]
    fn record_skips_an_exact_duplicate_row() {
        // G3: the same device, path, size and modified time recorded a
        // second time is the same file recorded held twice. It must not
        // grow the index, and it must not disturb the root count the first
        // recording added.
        let dir = temp_dir("exact-duplicate");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        store
            .record(row("device", "DCIM/a.jpg", 1024, 500, 7))
            .expect("the first record should append");
        store
            .record(row("device", "DCIM/a.jpg", 1024, 500, 7))
            .expect("the duplicate record must be a no-op, not an error");

        assert_eq!(store.len(), 1, "the duplicate must not add a second row");
        assert!(store.contains_path("device", "DCIM/a.jpg", 1024, 500));
        assert!(store.contains_root(&[7; 32]));

        let loaded = HeldStore::load(&path);
        assert_eq!(
            loaded.len(),
            1,
            "the file on disk holds one row, not a duplicate"
        );
    }

    #[test]
    fn dropping_the_oldest_row_does_not_lose_a_root_hash_a_surviving_row_shares() {
        // G3: `roots` counts rows per hash rather than a plain set, exactly
        // so that dropping one row sharing a root hash with another still
        // leaves that root hash found through the row that survives.
        let mut store = HeldStore::load(&temp_dir("shared-root").join("held"));
        store
            .record(row("device-1", "DCIM/a.jpg", 1024, 0, 9))
            .expect("the first record should append");
        store
            .record(row("device-2", "DCIM/b.jpg", 1024, 0, 9))
            .expect("the second record, sharing the same root hash, should append");

        // Fill up to exactly `MAX_ROWS`, so the next record is the one that
        // pushes past the bound and drops exactly the oldest row: the
        // first one recorded, above.
        for i in 0..(MAX_ROWS - 2) {
            let byte = u8::try_from(i % 256).unwrap_or(0);
            store
                .record(row("device-3", &format!("DCIM/{i}.jpg"), 1, 0, byte))
                .expect("filling to the bound should append");
        }
        assert_eq!(store.len(), MAX_ROWS);

        store
            .record(row("device-4", "DCIM/new.jpg", 1, 0, 255))
            .expect("the row that crosses the bound should rebuild");
        assert_eq!(store.len(), MAX_ROWS, "the bound is never crossed");

        assert!(
            !store.contains_path("device-1", "DCIM/a.jpg", 1024, 0),
            "the very first row is the oldest, and is the one dropped"
        );
        assert!(
            store.contains_root(&[9; 32]),
            "the second row's copy of the same root hash must still be found"
        );
    }

    #[test]
    fn contains_root_matches_across_devices_and_paths() {
        let mut store = HeldStore::load(&temp_dir("root-match").join("held"));
        store
            .record(row("device-1", "DCIM/a.jpg", 1024, 0, 7))
            .expect("the record should append");
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
            store
                .record(row("device", &format!("DCIM/{i}.jpg"), 1, 0, byte))
                .expect("the record should append");
        }
        assert_eq!(store.len(), MAX_ROWS);
        assert!(store.contains_path("device", "DCIM/0.jpg", 1, 0));
        assert_eq!(
            store.rewrites, 0,
            "every row up to the bound is an append, never a rewrite"
        );

        store
            .record(row("device", "DCIM/new.jpg", 1, 0, 255))
            .expect("the record should rebuild the file");
        assert_eq!(store.len(), MAX_ROWS, "the bound is never crossed");
        assert!(
            !store.contains_path("device", "DCIM/0.jpg", 1, 0),
            "the oldest row is the one dropped"
        );
        assert!(store.contains_path("device", "DCIM/new.jpg", 1, 0));
        assert_eq!(
            store.rewrites, 1,
            "crossing the bound is the one time this rebuilds the file"
        );
    }

    #[test]
    fn a_tampered_format_version_is_refused_and_loads_empty() {
        let dir = temp_dir("bad-version");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        store
            .record(row("device", "DCIM/a.jpg", 1, 0, 1))
            .expect("the record should append");

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

    #[test]
    fn fifty_completed_pulls_append_without_a_full_rewrite() {
        // G1: every completed pull used to rewrite and sync the whole
        // index. Now each one appends a single framed row, and only
        // rebuilds the whole file once the row count passes `MAX_ROWS`,
        // which fifty rows never comes close to.
        let dir = temp_dir("append-only");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        for i in 0..50u64 {
            let byte = u8::try_from(i).unwrap_or(0);
            store
                .record(row("device", &format!("DCIM/{i}.jpg"), 1, 0, byte))
                .expect("each of the fifty pulls should append");
        }
        assert_eq!(store.len(), 50);
        assert_eq!(
            store.rewrites, 0,
            "fifty completed pulls, all under the bound, only append"
        );

        let loaded = HeldStore::load(&path);
        assert_eq!(loaded.len(), 50, "every appended row survives a reload");
        assert!(loaded.contains_path("device", "DCIM/0.jpg", 1, 0));
        assert!(loaded.contains_path("device", "DCIM/49.jpg", 1, 0));
    }

    #[test]
    fn the_index_loads_after_a_torn_last_row() {
        // G1: a crash mid append can only ever leave one unfinished frame
        // at the end of this file, the same guarantee `access.rs` relies on
        // for a day file. `load` must still return every whole row before
        // it, and must cut the torn bytes away so the next append lands
        // right after the last good row.
        let dir = temp_dir("torn-tail");
        let path = dir.join("held");
        let mut store = HeldStore::load(&path);
        store
            .record(row("device", "DCIM/a.jpg", 1024, 0, 1))
            .expect("the first row should append");

        // A length prefix declaring one hundred bytes of content, with
        // nothing actually written after it: exactly what a crash between
        // writing a frame's length and its content leaves behind.
        let mut bytes = fs::read(&path).expect("the file should be there");
        let good_len = bytes.len();
        bytes.extend_from_slice(&100u32.to_be_bytes());
        fs::write(&path, &bytes).expect("the torn tail should write");

        let loaded = HeldStore::load(&path);
        assert_eq!(loaded.len(), 1, "the whole first row still loads");
        assert!(loaded.contains_path("device", "DCIM/a.jpg", 1024, 0));

        let repaired_len = fs::metadata(&path)
            .expect("the file should still exist")
            .len();
        assert_eq!(
            repaired_len,
            u64::try_from(good_len).unwrap_or(u64::MAX),
            "the torn tail is cut off, not left for the next append to land after"
        );
    }
}
