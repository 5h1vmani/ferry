//! The write-temp, fsync, rename pattern every store on disk in this crate
//! uses, in one place.
//!
//! `std::fs::write` truncates the file first and then writes. A process
//! that dies in the middle of that leaves a short file behind. So every
//! write here goes to a fresh temporary name next to the real path, is
//! flushed to disk, and only then is renamed over the real name. A rename
//! is one step, so the real path always holds either the whole old file or
//! the whole new one, never half of either. A failure at any step removes
//! the temporary file and reports `TransferError::Local`, because a
//! temporary file left behind is never read again but would otherwise sit
//! in the app's own folder for ever.
//!
//! `held.rs`, `auto_copy.rs`, and `networks.rs` want the file made private:
//! mode `0o600` on Unix, set as part of the same syscall that creates it,
//! so there is no moment where the file exists with a wider mode. They call
//! [`write_atomic_private`]. `record.rs` and `batch.rs` do not ask for
//! that, so they call [`write_atomic`], which keeps the process's ordinary
//! create mode.
//!
//! `ferry-core`'s `peers.rs` writes its own paired device list the same
//! way, but keeps its own copy of the pattern: that function is private to
//! the core crate, and this crate does not change the core.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ferry_core::session::SessionId;

use crate::FerryError;
use crate::errors::failed;

/// Write `bytes` to `path`, with the temporary file, and so the file it
/// becomes, made private: mode `0o600` on Unix, the same private mode
/// `ferry-core`'s `peers.rs` gives its own file.
pub(crate) fn write_atomic_private(path: &Path, bytes: &[u8]) -> Result<(), FerryError> {
    write_atomic_with(path, bytes, open_new_private_file)
}

/// Write `bytes` to `path`, with the temporary file kept at the process's
/// ordinary create mode.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), FerryError> {
    write_atomic_with(path, bytes, open_new_file)
}

/// The shared body of [`write_atomic`] and [`write_atomic_private`]. `open`
/// is the only difference between the two: which mode the temporary file is
/// created with.
fn write_atomic_with(
    path: &Path,
    bytes: &[u8],
    open: fn(&Path) -> std::io::Result<fs::File>,
) -> Result<(), FerryError> {
    let temporary = temporary_name(path)?;
    let written =
        write_and_sync(&temporary, bytes, open).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        drop(fs::remove_file(&temporary));
        return Err(failed("TransferError::Local"));
    }
    Ok(())
}

/// Create the file with `open`, write every byte through that one handle,
/// and flush it to the disk.
fn write_and_sync(
    path: &Path,
    bytes: &[u8],
    open: fn(&Path) -> std::io::Result<fs::File>,
) -> std::io::Result<()> {
    let mut file = open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Create `path`, refusing anything already there, including a symbolic
/// link someone planted there.
fn open_new_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// As [`open_new_file`], with the file made private: mode `0o600` on Unix,
/// set as part of the same syscall that creates it, so there is no moment
/// where the file exists with a wider mode.
#[cfg(unix)]
fn open_new_private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_new_private_file(path: &Path) -> std::io::Result<fs::File> {
    open_new_file(path)
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
