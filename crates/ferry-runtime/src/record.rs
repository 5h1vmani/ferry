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
//! the first byte, or stops for good. So the bytes go to a temporary name
//! chosen at random, are flushed to the disk, and only then renamed over the
//! real name. A rename is one step, so the real name always holds either the
//! whole old record or the whole new one.
//!
//! This is the pattern `write_private_file` in `ferry-core`'s `peers.rs`
//! uses for the paired device list. It is written again here rather than
//! shared, because that function is private to the core and this crate does
//! not change the core.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ferry_core::limits;
use ferry_core::path::RemotePath;
use ferry_core::session::{SessionId, Transfer};
use ferry_core::wire::{Decoder, Encoder};

use crate::FerryError;
use crate::errors::failed;

/// The version byte every record starts with.
const FORMAT_VERSION: u8 = 1;

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

/// One transfer record, in whichever stage it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Record {
    /// The manifest does not exist yet.
    FirstPass(FirstPass),
    /// The manifest is complete, so every later attempt is a resume.
    Ready(Transfer),
}

impl Record {
    /// The bytes to store.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION);
        match self {
            Self::FirstPass(pass) => {
                e.u8(STAGE_FIRST_PASS);
                e.text(pass.source.as_str());
                e.text(pass.destination.as_str());
                e.u64(pass.source_size);
                e.fixed(&pass.source_mtime.to_be_bytes());
                e.u32(pass.chunk_size);
                e.u32(pass.chunks_done);
            }
            Self::Ready(transfer) => {
                e.u8(STAGE_READY);
                e.bytes(&transfer.encode());
            }
        }
        e.finish()
    }

    /// Read a record back.
    ///
    /// # Errors
    ///
    /// Returns a `ManifestError::Wire` code for bytes this build cannot
    /// read, which covers a record from a newer format and a record a disk
    /// damaged.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, FerryError> {
        Self::decode_inner(bytes).ok_or_else(|| failed("ManifestError::Wire"))
    }

    /// The body of [`Record::decode`], where every step may simply fail.
    fn decode_inner(bytes: &[u8]) -> Option<Self> {
        let mut d = Decoder::new(bytes);
        if d.u8().ok()? != FORMAT_VERSION {
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
                Self::FirstPass(FirstPass {
                    source,
                    destination,
                    source_size,
                    source_mtime,
                    chunk_size,
                    chunks_done,
                })
            }
            STAGE_READY => {
                let inner = d
                    .bytes(limits::MAX_MANIFEST_BYTES + 2 * limits::MAX_PATH_LEN + 64)
                    .ok()?;
                Self::Ready(Transfer::decode(inner).ok()?)
            }
            _ => return None,
        };
        d.finish().ok()?;
        Some(record)
    }
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
        Ok(bytes) => Record::decode(&bytes).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(failed("TransferError::Local")),
    }
}

/// Write the record at `path`, so a crash can never leave a short one.
///
/// # Errors
///
/// Returns a `TransferError::Local` code when local storage refuses the
/// write, and a `TransferError::NoRandomness` code when the temporary name
/// cannot be made.
pub(crate) fn write_record(path: &Path, record: &Record) -> Result<(), FerryError> {
    let bytes = record.encode();
    let temporary = temporary_name(path)?;
    let written = write_and_sync(&temporary, &bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        // A temporary file that is left behind is never read again, but it
        // would sit in the app's own folder for ever.
        drop(fs::remove_file(&temporary));
        return Err(failed("TransferError::Local"));
    }
    Ok(())
}

/// Create the file, write every byte through that one handle, and flush it
/// to the disk.
///
/// `create_new` refuses to write through anything already at the name,
/// including a symbolic link someone planted there.
fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// A name next to `path` that nothing can be waiting at.
///
/// The random part comes from the same generator that names a transfer, so
/// this needs no new dependency.
fn temporary_name(path: &Path) -> Result<PathBuf, FerryError> {
    let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{session}.tmp"));
    Ok(PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::{FirstPass, Record, read_record, write_record};
    use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
    use ferry_core::path::RemotePath;
    use ferry_core::session::Transfer;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

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

    fn first_pass() -> Record {
        Record::FirstPass(FirstPass {
            source: path("holiday.bin"),
            destination: path("photos/holiday.bin"),
            source_size: 8 * 1024 * 1024,
            source_mtime: -12,
            chunk_size: 1024 * 1024,
            chunks_done: 2,
        })
    }

    fn ready() -> Record {
        let manifest = manifest_from_bytes(b"some bytes", ChunkSize::one_mebibyte());
        Record::Ready(
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
