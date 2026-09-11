//! The real filesystem, served through the file operations layer.
//!
//! Implemented against the contract below.
//!
//! # Contract
//!
//! `LocalFs` implements [`crate::rpc::FileOps`] over one shared root on disk.
//! It is the only thing that turns a peer's path into a real file, so every
//! rule in `docs/protocol.md` section 8 is enforced here and nowhere else.
//!
//! It is built on `cap_std::fs::Dir`, which resolves paths inside a directory
//! capability and cannot be talked into leaving it. That replaces hand-written
//! `O_NOFOLLOW` handling with a primitive that already does it correctly.
//!
//! Rules:
//!
//! 1. A path that resolves outside the root is refused, including through a
//!    symlink. `cap_std` guarantees this. A test proves it with a real symlink.
//! 2. Only regular files and directories are served. A FIFO, a socket, a
//!    device, or a symlink that reaches one is refused with
//!    [`crate::ops::OpError::Unsupported`]. For `read`, `write`, `truncate`,
//!    and `set_mtime`, this is checked on the opened handle, not on the path
//!    beforehand; see `open_checked`. Checking the path first and opening it
//!    a moment later left a gap for another local process to change what the
//!    path pointed to in between, which is how a FIFO could reach `read` and
//!    block its thread forever.
//! 3. `delete` is not recursive. A directory holding anything returns
//!    [`crate::ops::OpError::NotEmpty`].
//! 4. `rename` replaces the destination in one step.
//! 5. `read` past the end returns fewer bytes. `write` past the end extends
//!    with zeros. `truncate` shortens or extends.
//! 6. `list` pages at [`crate::limits::MAX_LIST_ENTRIES`], sorted by name, with
//!    the cursor as a zero-based index into the sorted children.
//! 7. Every std or `cap_std` error maps to one [`crate::ops::OpError`] variant.
//!    The mapping lives in one function.
//! 8. The empty path names the shared root. `list` and `stat` accept it.
//!    `read` and `write` refuse it with
//!    [`crate::ops::OpError::IsADirectory`]. That is the same code a real
//!    directory gets, because the root is one. `delete` and `rename` do
//!    accept a real directory, so their refusal is not about shape. Neither
//!    may touch the root, as either argument, so both refuse it with
//!    [`crate::ops::OpError::PermissionDenied`]. `mkdir` of the root refuses
//!    with [`crate::ops::OpError::AlreadyExists`], because the root is
//!    always already there. `truncate` needs no check of its own: opening
//!    the root as a file already fails, the same way it does for `read` and
//!    `write`. `set_mtime` still accepts the root, the same as any other
//!    directory.
//!
//! Public shape:
//!
//! ```text
//! pub struct LocalFs { .. }
//! impl LocalFs {
//!     /// Open a root. It must exist and be a directory.
//!     pub fn open(root: impl AsRef<std::path::Path>) -> Result<Self, OpError>;
//! }
//! impl FileOps for LocalFs { .. }
//! ```

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, Metadata, OpenOptions, OpenOptionsExt};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

use crate::chunk::{ChunkSize, Manifest, ManifestBuilder};
use crate::limits;
use crate::ops::{Entry, FileKind, OpError};
use crate::path::RemotePath;
use crate::rpc::FileOps;

/// A shared root on disk, served through [`FileOps`].
///
/// Every path a peer sends is resolved through the `cap_std::fs::Dir` held
/// here, never through `std::fs` and a joined path. That is what keeps a
/// peer inside the root.
#[derive(Debug)]
pub struct LocalFs {
    root: Dir,
    // See `ListCache`'s own documentation for what this holds and why.
    list_cache: Mutex<ListCache>,
}

impl LocalFs {
    /// Open a root. It must exist and be a directory.
    ///
    /// # Errors
    ///
    /// Returns an [`OpError`] built from the [`std::io::Error`] the open
    /// call failed with, most commonly [`OpError::NotFound`] when `root`
    /// does not exist.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, OpError> {
        let root = Dir::open_ambient_dir(root, ambient_authority()).map_err(|e| map_io(&e))?;
        Ok(Self {
            root,
            list_cache: Mutex::new(ListCache::default()),
        })
    }
}

/// Cached, already-sorted directory listings, one per path with a paging run
/// in progress.
///
/// `list` sorts a directory once, on the call that starts a paging run at
/// cursor `0`, and keeps the sorted result here so the later calls in the
/// same run can read the next page instead of reading and sorting the whole
/// directory again. That repeated read-and-sort was FINDING 4 of the audit
/// this module fixes: 118 ms a page at 60,000 entries, from 20 bytes of
/// request.
///
/// A listing is a snapshot for the life of one paging run. That is what a
/// paging cursor already means: a caller that keeps asking for the next page
/// is asking to keep walking the listing it was first handed, not a fresh
/// one that might have gained or lost entries in between calls.
#[derive(Debug, Default)]
struct ListCache {
    by_path: HashMap<String, Arc<Vec<Entry>>>,
    // Insertion order, oldest first. A `HashMap` keeps no order of its own,
    // and eviction needs to find the oldest entry once the cache is full.
    order: Vec<String>,
}

impl ListCache {
    // How many directories' listings are kept at once. Ferry pages one
    // directory, or occasionally two during a transfer, at a time in normal
    // use, so this is headroom, not a tight budget.
    const CAPACITY: usize = 8;

    fn get(&self, path: &str) -> Option<Arc<Vec<Entry>>> {
        self.by_path.get(path).cloned()
    }

    fn insert(&mut self, path: String, entries: Arc<Vec<Entry>>) {
        if !self.by_path.contains_key(&path) {
            if self.order.len() >= Self::CAPACITY {
                // `order[0]` is always the oldest entry, because a path is
                // only ever pushed onto the back, never reordered.
                let oldest = self.order.remove(0);
                self.by_path.remove(&oldest);
            }
            self.order.push(path.clone());
        }
        self.by_path.insert(path, entries);
    }

    fn remove(&mut self, path: &str) {
        self.by_path.remove(path);
        self.order.retain(|cached| cached != path);
    }
}

impl FileOps for LocalFs {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        // Cursor 0 always starts a fresh paging run, even when a listing for
        // this path is already cached. A stale run must never be handed
        // back as if it were the start of a new one.
        let cached = if cursor == 0 {
            None
        } else {
            self.list_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path.as_str())
        };

        // When there is no cached listing, cursor > 0 is treated the same
        // way cursor 0 is: the directory is read and sorted here, and the
        // page this call actually asked for is served out of the fresh
        // result below, not out of page 0.
        let children = if let Some(children) = cached {
            children
        } else {
            // `symlink_metadata` never follows the last component, so a
            // symlink left at `path` is classified as `Unsupported`, not as
            // whatever it points to.
            let dir_metadata = self
                .root
                .symlink_metadata(cap_std_path(path))
                .map_err(|e| map_io(&e))?;
            match classify(&dir_metadata)? {
                FileKind::Directory => {}
                FileKind::File => return Err(OpError::NotADirectory),
            }

            let mut entries = Vec::new();
            for entry in self
                .root
                .read_dir(cap_std_path(path))
                .map_err(|e| map_io(&e))?
            {
                let entry = entry.map_err(|e| map_io(&e))?;
                // One odd child must not fail the whole listing. A FIFO, a
                // socket, a device, or a symlink is left out instead.
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let Ok(kind) = classify(&metadata) else {
                    continue;
                };
                // A name that is not valid UTF-8 cannot round trip through
                // the wire format, which carries every name as UTF-8 text.
                // `to_string_lossy` would show it anyway, under a name that
                // maps to no real path, and two different real names could
                // then collide on the same lossy name. Skipping it is the
                // honest answer: Ferry cannot show a name it cannot carry.
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                entries.push(Entry {
                    name,
                    kind,
                    size: if kind == FileKind::File {
                        metadata.len()
                    } else {
                        0
                    },
                    modified_unix_secs: modified_secs(&metadata),
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Arc::new(entries)
        };

        let page = usize::try_from(limits::MAX_LIST_ENTRIES).unwrap_or(usize::MAX);
        let start = usize::try_from(cursor)
            .unwrap_or(usize::MAX)
            .min(children.len());
        let end = start.saturating_add(page).min(children.len());
        let next_cursor = if end < children.len() {
            Some(u64::try_from(end).unwrap_or(u64::MAX))
        } else {
            None
        };
        let page_entries = children[start..end].to_vec();

        let mut cache = self
            .list_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if next_cursor.is_some() {
            cache.insert(path.as_str().to_owned(), children);
        } else {
            // The last page was just served. Nothing will ask for this path
            // with a non-zero cursor again until a fresh run starts at 0, so
            // the snapshot is dropped now instead of waiting to be evicted.
            cache.remove(path.as_str());
        }

        Ok((page_entries, next_cursor))
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        let metadata = self
            .root
            .symlink_metadata(cap_std_path(path))
            .map_err(|e| map_io(&e))?;
        let kind = classify(&metadata)?;
        Ok(Entry {
            name: leaf_name(path),
            kind,
            size: if kind == FileKind::File {
                metadata.len()
            } else {
                0
            },
            modified_unix_secs: modified_secs(&metadata),
        })
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if length > limits::MAX_READ_LEN {
            return Err(OpError::RangeTooLarge);
        }
        // The root is a directory, and `read` only ever serves a file. The
        // check below would catch this once it opened the root, but saying
        // so here is clearer than waiting on that.
        if path.is_root() {
            return Err(OpError::IsADirectory);
        }

        // No `symlink_metadata` call runs first. See `open_checked` for why:
        // checking what is at `path` and opening it are one step now, not
        // two, so there is no gap in between for another process to swap
        // what `path` points at.
        let (mut file, kind) = open_checked(
            &self.root,
            cap_std_path(path),
            OpenOptions::new().read(true),
        )?;
        if kind == FileKind::Directory {
            return Err(OpError::IsADirectory);
        }

        file.seek(SeekFrom::Start(offset)).map_err(|e| map_io(&e))?;
        let mut bytes = Vec::new();
        file.take(u64::from(length))
            .read_to_end(&mut bytes)
            .map_err(|e| map_io(&e))?;
        Ok(bytes)
    }

    fn write(&self, path: &RemotePath, offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        let written = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        if written > limits::MAX_WRITE_LEN {
            return Err(OpError::RangeTooLarge);
        }
        // The root is a directory, and `write` only ever serves a file. Same
        // reasoning as `read`, above.
        if path.is_root() {
            return Err(OpError::IsADirectory);
        }

        // `create(true)` covers the case a separate check used to handle by
        // hand: a path with nothing at it yet, which `write` is allowed to
        // fill in. `open_checked` still classifies whatever handle comes
        // back, so a directory, a FIFO, a socket, or a device already there
        // is refused the same way as in every other operation here, from
        // what was actually opened.
        let (mut file, kind) = open_checked(
            &self.root,
            cap_std_path(path),
            OpenOptions::new().write(true).create(true),
        )?;
        if kind == FileKind::Directory {
            return Err(OpError::IsADirectory);
        }

        file.seek(SeekFrom::Start(offset)).map_err(|e| map_io(&e))?;
        file.write_all(bytes).map_err(|e| map_io(&e))?;
        Ok(written)
    }

    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError> {
        // No explicit root check is needed here. `open_checked` opens the
        // root as a directory (see `cap_std_path`), and the check just below
        // already refuses any directory, root or not, the same way `read`
        // and `write` refuse it up front.
        let (file, kind) = open_checked(
            &self.root,
            cap_std_path(path),
            OpenOptions::new().write(true),
        )?;
        if kind == FileKind::Directory {
            return Err(OpError::IsADirectory);
        }
        file.set_len(length).map_err(|e| map_io(&e))
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError> {
        // `rename` does accept a real directory, so the refusal here is not
        // about shape the way `read` and `write`'s is. It is refused because
        // the root itself may neither be moved away nor be overwritten by
        // something else, so both arguments are checked before either side
        // of the capability directory is touched.
        if from.is_root() || to.is_root() {
            return Err(OpError::PermissionDenied);
        }
        let metadata = self
            .root
            .symlink_metadata(cap_std_path(from))
            .map_err(|e| map_io(&e))?;
        classify(&metadata)?;
        self.root
            .rename(cap_std_path(from), &self.root, cap_std_path(to))
            .map_err(|e| map_io(&e))
    }

    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError> {
        // Unlike `read`, `write`, and `truncate`, this is allowed to land on
        // a directory; `Request::SetMtime`'s contract covers both, and that
        // includes the root, which is a directory like any other for this
        // one operation. A plain read-only open is enough either way:
        // setting a file's or a directory's time depends on ownership and
        // permission, not on how the handle was opened. `open_checked`'s
        // `classify` call still refuses a FIFO, a socket, or a device, which
        // is the only thing that mattered for the race this function used to
        // have.
        let (file, _kind) = open_checked(
            &self.root,
            cap_std_path(path),
            OpenOptions::new().read(true),
        )?;
        file.into_std()
            .set_modified(unix_secs_to_system_time(modified_unix_secs))
            .map_err(|e| map_io(&e))
    }

    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError> {
        // The root is always already there, so creating it again is refused
        // the same way creating any other existing directory is: the target
        // already exists.
        if path.is_root() {
            return Err(OpError::AlreadyExists);
        }
        self.root
            .create_dir(cap_std_path(path))
            .map_err(|e| map_io(&e))
    }

    fn delete(&self, path: &RemotePath) -> Result<(), OpError> {
        // `delete` does accept a real directory, so, as with `rename`, the
        // refusal here is not about shape. The root may never be removed,
        // empty or not.
        if path.is_root() {
            return Err(OpError::PermissionDenied);
        }
        let metadata = self
            .root
            .symlink_metadata(cap_std_path(path))
            .map_err(|e| map_io(&e))?;
        match classify(&metadata)? {
            FileKind::File => self
                .root
                .remove_file(cap_std_path(path))
                .map_err(|e| map_io(&e)),
            // `remove_dir` is a plain `rmdir`. It fails with `NotEmpty`
            // instead of taking anything down with it, because delete is not
            // recursive.
            FileKind::Directory => self
                .root
                .remove_dir(cap_std_path(path))
                .map_err(|e| map_io(&e)),
        }
    }

    fn manifest(&self, path: &RemotePath) -> Result<Manifest, OpError> {
        // The root is a directory, and a manifest only ever describes a
        // file. Same reasoning as `read` and `write`, above.
        if path.is_root() {
            return Err(OpError::IsADirectory);
        }
        let (mut file, kind) = open_checked(
            &self.root,
            cap_std_path(path),
            OpenOptions::new().read(true),
        )?;
        if kind == FileKind::Directory {
            return Err(OpError::IsADirectory);
        }

        // The length comes from the open handle, not from reading the file,
        // so a file too large to describe is refused before the first byte
        // is read. `manifest_chunk_size` also picks a chunk size larger than
        // the one mebibyte default when the length needs it, so the chunk
        // count never overflows `MAX_MANIFEST_CHUNKS`.
        let length = file.metadata().map_err(|e| map_io(&e))?.len();
        let chunk_size = manifest_chunk_size(length)?;

        // One pass over the file, in chunk sized pieces, through the same
        // `ManifestBuilder` a transfer's first pass uses. Every piece but
        // the last is a whole chunk; `read_to_end` on a bounded `take`
        // hands back a short final piece on its own, with no extra check
        // needed here.
        let mut builder = ManifestBuilder::new(chunk_size);
        loop {
            let mut piece = Vec::new();
            // `File` implements both `Read` and `Write`, so a plain
            // `file.by_ref()` is ambiguous between the two; the explicit
            // trait name picks the one meant here.
            Read::by_ref(&mut file)
                .take(chunk_size.as_u64())
                .read_to_end(&mut piece)
                .map_err(|e| map_io(&e))?;
            if piece.is_empty() {
                break;
            }
            let whole_chunk = piece.len() as u64 == chunk_size.as_u64();
            builder.push(&piece);
            if !whole_chunk {
                break;
            }
        }
        Ok(builder.finish())
    }
}

// `cap_std::fs::Dir` has no concept of "the root itself" as a path string;
// every one of its methods takes a path relative to the capability, and an
// empty string is not a valid one. `.` names the same directory in every
// `cap_std` call, so the root maps to that here, and every other path is
// unchanged. Every call into `self.root` above goes through this function,
// so a path that reaches `cap_std` is never the bare empty string.
fn cap_std_path(path: &RemotePath) -> &str {
    if path.is_root() { "." } else { path.as_str() }
}

// `custom_flags` takes an `i32`, and `OFlags::bits` is a `u32`. The value is
// a single flag bit, so reinterpreting the bytes is exact. This mirrors how
// ops.rs carries an `i64` inside a `u64`.
const O_NONBLOCK: i32 = i32::from_ne_bytes(OFlags::NONBLOCK.bits().to_ne_bytes());

// Opens `path` and classifies what got opened, instead of classifying the
// path and then opening it as two separate calls. Two calls leave a gap: a
// local process can change what sits at `path` between them, so the
// classification and the open can end up looking at different objects. A
// FIFO swapped in during that gap is FINDING 1 of the audit this fixes: the
// old `read` checked the path, then opened it a moment later, and an opened
// FIFO with no writer blocks the thread forever.
//
// Opening first and classifying the handle closes the gap, because there is
// nothing left to swap once the handle exists: whatever `classify` reports
// here is the exact object every caller of this function goes on to read,
// write, or set the time of.
//
// `O_NONBLOCK` is what keeps the open itself from being the hang. A FIFO
// opened for reading blocks until a writer connects, and one opened for
// writing blocks until a reader connects, unless this flag is set, in which
// case the call returns at once instead, successfully or with an error, but
// never by waiting. The flag is left set on the handle afterward rather than
// cleared with `fcntl`: by the time any caller uses the handle, `classify`
// has already confirmed it is a regular file or a directory, and POSIX
// defines `O_NONBLOCK` as having no effect on a regular file's `read`,
// `write`, or `set_len`. Clearing it would need a raw `fcntl` call, which
// needs `libc` or `rustix` as a direct dependency (neither is one, see the
// constant above) or a hand-written FFI declaration, which the workspace's
// `unsafe_code = "deny"` lint forbids outright.
// The largest file a manifest can describe at all: `MAX_MANIFEST_CHUNKS`
// chunks of the largest chunk size BLAKE3 accepts. Above this, no chunk
// size keeps the chunk count within the limit, so `manifest` refuses the
// file outright, before it opens it for reading.
const MAX_MANIFEST_FILE_LEN: u64 = limits::MAX_MANIFEST_CHUNKS as u64 * ChunkSize::MAX as u64;

// Picks the smallest chunk size that keeps a file of `length` bytes at or
// under `MAX_MANIFEST_CHUNKS` chunks, starting from the one mebibyte
// default. A file at or below 32 GiB keeps that default; a larger file
// (up to 512 GiB) steps up to the next power of two chunk size as needed.
fn manifest_chunk_size(length: u64) -> Result<ChunkSize, OpError> {
    if length > MAX_MANIFEST_FILE_LEN {
        return Err(OpError::RangeTooLarge);
    }
    let mut chunk_size = ChunkSize::one_mebibyte();
    while length.div_ceil(chunk_size.as_u64()) > u64::from(limits::MAX_MANIFEST_CHUNKS) {
        chunk_size = ChunkSize::new(chunk_size.get() * 2)
            .expect("doubling a valid chunk size below MAX stays a valid power of two");
    }
    Ok(chunk_size)
}

fn open_checked(
    root: &Dir,
    path: &str,
    options: &mut OpenOptions,
) -> Result<(File, FileKind), OpError> {
    options.custom_flags(O_NONBLOCK);
    let file = root.open_with(path, options).map_err(|e| map_io(&e))?;
    let metadata = file.metadata().map_err(|e| map_io(&e))?;
    let kind = classify(&metadata)?;
    // The handle is now known to be a regular file or a directory, so a
    // blocking read cannot hang on it. Clear the flag so the handle behaves
    // like any other from here on.
    let flags = fcntl_getfl(&file).map_err(|_| OpError::Internal)?;
    fcntl_setfl(&file, flags - OFlags::NONBLOCK).map_err(|_| OpError::Internal)?;
    Ok((file, kind))
}

// The only place that turns an I/O failure into an `OpError`. Every function
// above calls this instead of matching an `io::ErrorKind` itself. It takes a
// reference because `Result::map_err` always hands the error over by value,
// and a reference is all this function needs to read the kind.
fn map_io(error: &io::Error) -> OpError {
    match error.kind() {
        io::ErrorKind::NotFound => OpError::NotFound,
        io::ErrorKind::AlreadyExists => OpError::AlreadyExists,
        io::ErrorKind::PermissionDenied => OpError::PermissionDenied,
        io::ErrorKind::DirectoryNotEmpty => OpError::NotEmpty,
        io::ErrorKind::IsADirectory => OpError::IsADirectory,
        io::ErrorKind::NotADirectory => OpError::NotADirectory,
        _ => OpError::Internal,
    }
}

// A regular file or a directory becomes an `Entry`. Anything else, a FIFO, a
// socket, a device, or a symlink, is refused here, once, instead of at every
// call site.
fn classify(metadata: &Metadata) -> Result<FileKind, OpError> {
    if metadata.is_dir() {
        Ok(FileKind::Directory)
    } else if metadata.is_file() {
        Ok(FileKind::File)
    } else {
        Err(OpError::Unsupported)
    }
}

// The last component of a path, for the `name` field of an `Entry`.
// `RemotePath::components` only promises a forward `Iterator`, so the last
// component is read directly off the string instead. Every path but the root
// has at least one component, so the fallback is never actually used for
// those; it only keeps this function free of an `unwrap`. For the root,
// whose string form is empty, `rsplit` on an empty string still yields one
// empty piece, so this already returns the empty string without needing a
// case of its own. See `Entry::name` in `crate::ops` for what that means.
fn leaf_name(path: &RemotePath) -> String {
    path.as_str()
        .rsplit('/')
        .next()
        .unwrap_or(path.as_str())
        .to_string()
}

// `Entry::modified_unix_secs` is 0 when the platform cannot report a
// modification time at all. That is rare, and a peer would rather see a
// placeholder than have the whole call fail over it.
fn modified_secs(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .map_or(0, |time| system_time_to_unix_secs(time.into_std()))
}

fn system_time_to_unix_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since_epoch) => i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX),
        Err(before_epoch) => {
            let secs = before_epoch.duration().as_secs();
            i64::try_from(secs).map_or(i64::MIN, |secs| -secs)
        }
    }
}

fn unix_secs_to_system_time(secs: i64) -> SystemTime {
    let magnitude = Duration::from_secs(secs.unsigned_abs());
    if secs >= 0 {
        UNIX_EPOCH.checked_add(magnitude).unwrap_or(UNIX_EPOCH)
    } else {
        UNIX_EPOCH.checked_sub(magnitude).unwrap_or(UNIX_EPOCH)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use super::LocalFs;
    use crate::chunk::{ChunkSize, manifest_from_bytes};
    use crate::limits;
    use crate::ops::{FileKind, OpError};
    use crate::path::RemotePath;
    use crate::rpc::FileOps;

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    /// A directory under the system temp directory, unique to one test, that
    /// removes itself when the test ends, including when the test panics.
    struct TempRoot {
        dir: PathBuf,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ferry-localfs-{label}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn fs(&self) -> LocalFs {
            LocalFs::open(&self.dir).unwrap()
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn mkdir_then_stat_shows_a_directory() {
        let root = TempRoot::new("mkdir");
        let fs = root.fs();
        fs.mkdir(&path("NewFolder")).unwrap();
        let entry = fs.stat(&path("NewFolder")).unwrap();
        assert_eq!(entry.kind, FileKind::Directory);
        assert_eq!(entry.size, 0);
    }

    #[test]
    fn mkdir_on_an_existing_path_is_already_exists() {
        let root = TempRoot::new("mkdir-exists");
        let fs = root.fs();
        fs.mkdir(&path("NewFolder")).unwrap();
        assert_eq!(fs.mkdir(&path("NewFolder")), Err(OpError::AlreadyExists));
    }

    #[test]
    fn write_then_read_round_trips_the_same_bytes() {
        let root = TempRoot::new("write-read");
        let fs = root.fs();
        let written = fs.write(&path("a.txt"), 0, b"hello world").unwrap();
        assert_eq!(written, 11);
        let bytes = fs.read(&path("a.txt"), 0, 11).unwrap();
        assert_eq!(bytes, b"hello world");
    }

    #[test]
    fn write_then_stat_shows_size_and_kind() {
        let root = TempRoot::new("write-stat");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hello").unwrap();
        let entry = fs.stat(&path("a.txt")).unwrap();
        assert_eq!(entry.name, "a.txt");
        assert_eq!(entry.kind, FileKind::File);
        assert_eq!(entry.size, 5);
    }

    #[test]
    fn stat_on_a_missing_path_is_not_found() {
        let root = TempRoot::new("stat-missing");
        let fs = root.fs();
        assert_eq!(fs.stat(&path("nope.txt")), Err(OpError::NotFound));
    }

    #[test]
    fn list_returns_files_and_directories_sorted_by_name() {
        let root = TempRoot::new("list-sorted");
        let fs = root.fs();
        fs.mkdir(&path("DCIM")).unwrap();
        fs.write(&path("DCIM/b.jpg"), 0, b"b").unwrap();
        fs.write(&path("DCIM/a.jpg"), 0, b"a").unwrap();
        fs.mkdir(&path("DCIM/Sub")).unwrap();

        let (entries, next_cursor) = fs.list(&path("DCIM"), 0).unwrap();
        assert_eq!(next_cursor, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Sub", "a.jpg", "b.jpg"]);
    }

    #[test]
    fn list_on_a_file_is_not_a_directory() {
        let root = TempRoot::new("list-on-file");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hi").unwrap();
        assert_eq!(fs.list(&path("a.txt"), 0), Err(OpError::NotADirectory));
    }

    #[test]
    fn list_on_a_missing_path_is_not_found() {
        let root = TempRoot::new("list-missing");
        let fs = root.fs();
        assert_eq!(fs.list(&path("nope"), 0), Err(OpError::NotFound));
    }

    #[test]
    fn list_of_the_root_lists_the_shared_root() {
        let root = TempRoot::new("list-root");
        let fs = root.fs();
        fs.mkdir(&path("DCIM")).unwrap();
        fs.write(&path("a.txt"), 0, b"hi").unwrap();

        let (entries, next_cursor) = fs.list(&path(""), 0).unwrap();
        assert_eq!(next_cursor, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["DCIM", "a.txt"]);
    }

    #[test]
    fn stat_of_the_root_answers_a_directory_entry_with_an_empty_name() {
        let root = TempRoot::new("stat-root");
        let fs = root.fs();
        let entry = fs.stat(&path("")).unwrap();
        assert_eq!(entry.name, "");
        assert_eq!(entry.kind, FileKind::Directory);
    }

    #[test]
    fn a_write_to_the_root_is_refused() {
        let root = TempRoot::new("write-root");
        let fs = root.fs();
        assert_eq!(fs.write(&path(""), 0, b"hi"), Err(OpError::IsADirectory));
    }

    #[test]
    fn list_pages_at_the_entry_limit_using_the_cursor() {
        let root = TempRoot::new("list-paging");
        let fs = root.fs();
        fs.mkdir(&path("many")).unwrap();
        let total = limits::MAX_LIST_ENTRIES as usize + 3;
        for i in 0..total {
            fs.write(&path(&format!("many/{i:05}")), 0, b"").unwrap();
        }

        let (first_page, next_cursor) = fs.list(&path("many"), 0).unwrap();
        assert_eq!(first_page.len(), limits::MAX_LIST_ENTRIES as usize);
        let next_cursor = next_cursor.expect("more entries remain");

        let (second_page, next_cursor) = fs.list(&path("many"), next_cursor).unwrap();
        assert_eq!(second_page.len(), 3);
        assert_eq!(next_cursor, None);
    }

    #[test]
    fn read_past_the_end_returns_fewer_bytes() {
        let root = TempRoot::new("read-short");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hello").unwrap();
        let bytes = fs.read(&path("a.txt"), 3, 10).unwrap();
        assert_eq!(bytes, b"lo");
    }

    #[test]
    fn read_entirely_past_the_end_returns_empty() {
        let root = TempRoot::new("read-empty");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hello").unwrap();
        let bytes = fs.read(&path("a.txt"), 50, 10).unwrap();
        assert_eq!(bytes, Vec::<u8>::new());
    }

    #[test]
    fn read_of_a_directory_is_is_a_directory() {
        let root = TempRoot::new("read-dir");
        let fs = root.fs();
        fs.mkdir(&path("DCIM")).unwrap();
        assert_eq!(fs.read(&path("DCIM"), 0, 10), Err(OpError::IsADirectory));
    }

    #[test]
    fn read_over_the_length_limit_is_range_too_large() {
        let root = TempRoot::new("read-too-large");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hello").unwrap();
        assert_eq!(
            fs.read(&path("a.txt"), 0, limits::MAX_READ_LEN + 1),
            Err(OpError::RangeTooLarge)
        );
    }

    #[test]
    fn write_past_the_end_extends_with_zeros() {
        let root = TempRoot::new("write-extend");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hi").unwrap();
        fs.write(&path("a.txt"), 5, b"z").unwrap();
        let bytes = fs.read(&path("a.txt"), 0, 6).unwrap();
        assert_eq!(bytes, b"hi\0\0\0z");
    }

    #[test]
    fn write_to_a_directory_is_is_a_directory() {
        let root = TempRoot::new("write-dir");
        let fs = root.fs();
        fs.mkdir(&path("DCIM")).unwrap();
        assert_eq!(
            fs.write(&path("DCIM"), 0, b"hi"),
            Err(OpError::IsADirectory)
        );
    }

    #[test]
    fn write_over_the_length_limit_is_range_too_large() {
        let root = TempRoot::new("write-too-large");
        let fs = root.fs();
        let big = vec![0u8; (limits::MAX_WRITE_LEN + 1) as usize];
        assert_eq!(
            fs.write(&path("a.txt"), 0, &big),
            Err(OpError::RangeTooLarge)
        );
    }

    #[test]
    fn manifest_equals_the_one_manifest_builder_gives_over_the_same_bytes() {
        // docs/engine-contract.md item 16a: the serving side reads the whole
        // file once and hashes it with the existing `ManifestBuilder`. This
        // checks `LocalFs::manifest` against that same builder run over the
        // identical bytes, at a length that crosses several chunk
        // boundaries and ends with a short one.
        let root = TempRoot::new("manifest");
        let bytes: Vec<u8> = (0..(3 * limits::MAX_READ_LEN as usize + 12345))
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        // `LocalFs::write` caps one call at `MAX_WRITE_LEN`, so a file this
        // size is written directly rather than through the trait.
        std::fs::write(root.dir.join("a.bin"), &bytes).unwrap();
        let fs = root.fs();

        let served = fs.manifest(&path("a.bin")).unwrap();
        let expected = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
        assert_eq!(served, expected);
    }

    #[test]
    #[ignore = "runs on demand: hashes a 32 GiB sparse file"]
    fn manifest_of_a_file_just_over_32_gib_uses_a_larger_chunk_size() {
        // F1: at the one mebibyte default, a file over 32 GiB would need
        // more than `MAX_MANIFEST_CHUNKS` chunks. `manifest` must pick a
        // larger chunk size instead of overflowing the limit. The file is
        // sparse: `set_len` alone never writes a byte of it.
        let root = TempRoot::new("manifest-32gib");
        let over_32_gib = 32 * 1024 * 1024 * 1024 + 1;
        let file = std::fs::File::create(root.dir.join("big.bin")).unwrap();
        file.set_len(over_32_gib).unwrap();
        drop(file);
        let fs = root.fs();

        let manifest = fs.manifest(&path("big.bin")).unwrap();
        assert_eq!(manifest.length(), over_32_gib);
        assert!(
            manifest.chunk_size().as_u64() > ChunkSize::one_mebibyte().as_u64(),
            "a file over 32 GiB must use a chunk size larger than the default"
        );
        assert!(
            u32::try_from(manifest.chunk_count()).unwrap_or(u32::MAX)
                <= limits::MAX_MANIFEST_CHUNKS
        );
    }

    #[test]
    fn manifest_of_a_file_over_512_gib_is_refused_before_any_read() {
        // F1: 512 GiB is the largest length a manifest can describe at
        // `ChunkSize::MAX`. Above that, `manifest` must refuse before it
        // reads a single byte, so this returns quickly even though the
        // file is sparse and never actually holds 512 GiB on disk.
        let root = TempRoot::new("manifest-too-large");
        let over_512_gib = 512u64 * 1024 * 1024 * 1024 + 1;
        let file = std::fs::File::create(root.dir.join("huge.bin")).unwrap();
        file.set_len(over_512_gib).unwrap();
        drop(file);
        let fs = root.fs();

        let started = Instant::now();
        assert_eq!(fs.manifest(&path("huge.bin")), Err(OpError::RangeTooLarge));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a length over 512 GiB must be refused before any read, not after one"
        );
    }

    #[test]
    fn manifest_of_an_empty_file_matches_an_empty_builder() {
        let root = TempRoot::new("manifest-empty");
        let fs = root.fs();
        fs.write(&path("empty.bin"), 0, &[]).unwrap();

        let served = fs.manifest(&path("empty.bin")).unwrap();
        let expected = manifest_from_bytes(&[], ChunkSize::one_mebibyte());
        assert_eq!(served, expected);
    }

    #[test]
    fn manifest_of_a_directory_is_a_directory() {
        let root = TempRoot::new("manifest-dir");
        let fs = root.fs();
        fs.mkdir(&path("DCIM")).unwrap();
        assert_eq!(fs.manifest(&path("DCIM")), Err(OpError::IsADirectory));
    }

    #[test]
    fn manifest_of_a_missing_path_is_not_found() {
        let root = TempRoot::new("manifest-missing");
        let fs = root.fs();
        assert_eq!(fs.manifest(&path("nope.bin")), Err(OpError::NotFound));
    }

    #[test]
    fn truncate_shortens_a_file() {
        let root = TempRoot::new("truncate-short");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hello").unwrap();
        fs.truncate(&path("a.txt"), 2).unwrap();
        let bytes = fs.read(&path("a.txt"), 0, 10).unwrap();
        assert_eq!(bytes, b"he");
    }

    #[test]
    fn truncate_extends_a_file_with_zeros() {
        let root = TempRoot::new("truncate-extend");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hi").unwrap();
        fs.truncate(&path("a.txt"), 4).unwrap();
        let bytes = fs.read(&path("a.txt"), 0, 10).unwrap();
        assert_eq!(bytes, b"hi\0\0");
    }

    #[test]
    fn truncate_on_a_missing_path_is_not_found() {
        let root = TempRoot::new("truncate-missing");
        let fs = root.fs();
        assert_eq!(fs.truncate(&path("nope"), 0), Err(OpError::NotFound));
    }

    #[test]
    fn set_mtime_then_stat_shows_the_new_time() {
        let root = TempRoot::new("set-mtime");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hi").unwrap();
        fs.set_mtime(&path("a.txt"), 12345).unwrap();
        let entry = fs.stat(&path("a.txt")).unwrap();
        assert_eq!(entry.modified_unix_secs, 12345);
    }

    #[test]
    fn set_mtime_on_a_missing_path_is_not_found() {
        let root = TempRoot::new("set-mtime-missing");
        let fs = root.fs();
        assert_eq!(fs.set_mtime(&path("nope"), 1), Err(OpError::NotFound));
    }

    #[test]
    fn rename_replaces_an_existing_destination() {
        let root = TempRoot::new("rename");
        let fs = root.fs();
        fs.write(&path("old.txt"), 0, b"one").unwrap();
        fs.write(&path("new.txt"), 0, b"two").unwrap();
        fs.rename(&path("old.txt"), &path("new.txt")).unwrap();
        assert_eq!(fs.read(&path("new.txt"), 0, 10).unwrap(), b"one");
        assert_eq!(fs.stat(&path("old.txt")), Err(OpError::NotFound));
    }

    #[test]
    fn rename_of_a_missing_source_is_not_found() {
        let root = TempRoot::new("rename-missing");
        let fs = root.fs();
        assert_eq!(
            fs.rename(&path("nope"), &path("also-nope")),
            Err(OpError::NotFound)
        );
    }

    #[test]
    fn delete_of_a_file_works() {
        let root = TempRoot::new("delete-file");
        let fs = root.fs();
        fs.write(&path("a.txt"), 0, b"hi").unwrap();
        fs.delete(&path("a.txt")).unwrap();
        assert_eq!(fs.stat(&path("a.txt")), Err(OpError::NotFound));
    }

    #[test]
    fn delete_of_an_empty_directory_works() {
        let root = TempRoot::new("delete-empty-dir");
        let fs = root.fs();
        fs.mkdir(&path("Empty")).unwrap();
        fs.delete(&path("Empty")).unwrap();
        assert_eq!(fs.stat(&path("Empty")), Err(OpError::NotFound));
    }

    #[test]
    fn delete_of_a_non_empty_directory_is_not_empty() {
        let root = TempRoot::new("delete-non-empty-dir");
        let fs = root.fs();
        fs.mkdir(&path("DCIM")).unwrap();
        fs.write(&path("DCIM/a.jpg"), 0, b"a").unwrap();
        assert_eq!(fs.delete(&path("DCIM")), Err(OpError::NotEmpty));
    }

    #[test]
    fn delete_of_a_missing_path_is_not_found() {
        let root = TempRoot::new("delete-missing");
        let fs = root.fs();
        assert_eq!(fs.delete(&path("nope")), Err(OpError::NotFound));
    }

    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let root = TempRoot::new("symlink-root");
        let outside = TempRoot::new("symlink-outside");
        std::fs::write(outside.dir.join("secret.txt"), b"outside-secret").unwrap();
        std::os::unix::fs::symlink(&outside.dir, root.dir.join("link")).unwrap();

        let fs = root.fs();

        // The symlink itself is the last component here, and `symlink_metadata`
        // never follows the last component. It is classified as neither a file
        // nor a directory, so both calls are refused before anything is opened.
        assert_eq!(fs.stat(&path("link")), Err(OpError::Unsupported));
        assert_eq!(fs.list(&path("link"), 0), Err(OpError::Unsupported));

        // "link/secret.txt" reaches through the symlink as a component in the
        // middle of the path, not the last one. cap_std's resolver notices
        // that following it would leave the root and refuses with a
        // dedicated "a path led outside of the filesystem" I/O error, which
        // `map_io` reports as `PermissionDenied`. Either that or `NotFound`
        // is an honest refusal; what must never happen is the call
        // succeeding and handing back the secret bytes.
        match fs.stat(&path("link/secret.txt")) {
            Err(OpError::NotFound | OpError::Unsupported | OpError::PermissionDenied) => {}
            other => panic!("expected the escape to be refused, got {other:?}"),
        }
        match fs.read(&path("link/secret.txt"), 0, 64) {
            Err(OpError::NotFound | OpError::Unsupported | OpError::PermissionDenied) => {}
            Ok(bytes) => panic!("a symlink escaped the root and returned {bytes:?}"),
            Err(other) => panic!("expected the escape to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_fifo_inside_the_root_is_refused_and_skipped_by_list() {
        let root = TempRoot::new("fifo");
        std::fs::create_dir(root.dir.join("sub")).unwrap();
        std::fs::write(root.dir.join("sub/a.txt"), b"hi").unwrap();
        let fifo_path = root.dir.join("sub/pipe");

        // `libc` is not a dependency of this crate, so the FIFO is made with
        // the system `mkfifo` binary instead of `libc::mkfifo`. If that
        // binary is not on the machine running the test, the FIFO-specific
        // checks are skipped rather than failed.
        let made_fifo = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .is_ok_and(|status| status.success());
        if !made_fifo {
            eprintln!("mkfifo is not available here; skipping the FIFO checks");
            return;
        }

        let fs = root.fs();
        assert_eq!(fs.stat(&path("sub/pipe")), Err(OpError::Unsupported));
        // `read` opens this FIFO with `O_NONBLOCK` (see `open_checked`), so
        // the open returns at once instead of waiting for a writer, and the
        // handle is then refused because it is not a regular file.
        assert_eq!(fs.read(&path("sub/pipe"), 0, 10), Err(OpError::Unsupported));

        let (entries, next_cursor) = fs.list(&path("sub"), 0).unwrap();
        assert_eq!(next_cursor, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt"]);
    }

    #[test]
    fn a_fifo_swapped_in_after_the_check_does_not_block() {
        let root = TempRoot::new("fifo-race");
        let target_name = "target.bin";
        std::fs::write(root.dir.join(target_name), b"hello").unwrap();

        // `mkfifo` availability is checked once, the same way the other FIFO
        // test above does it. If it is missing, this test cannot run the
        // race it exists to check, so it is skipped rather than failed.
        let probe_path = root.dir.join("mkfifo-probe");
        let made_fifo = std::process::Command::new("mkfifo")
            .arg(&probe_path)
            .status()
            .is_ok_and(|status| status.success());
        if !made_fifo {
            eprintln!("mkfifo is not available here; skipping the FIFO race check");
            return;
        }
        std::fs::remove_file(&probe_path).unwrap();

        // This is FINDING 1 of the audit `localfs.rs` fixes. The old `read`
        // checked what was at a path with `symlink_metadata`, then opened
        // that same path a moment later, as two separate calls. A local
        // process that swaps a FIFO in between the two calls makes the
        // second one open a FIFO with no writer, which blocks the calling
        // thread forever; the auditor's reproduction hung after 1017 reads.
        // Reading the fixed code cannot prove the race is closed. Only
        // racing the two calls against a real swap does, so this test swaps
        // the target between a regular file and a FIFO on one thread while
        // reading it on another, thousands of times, against a deadline.
        let keep_swapping = Arc::new(AtomicBool::new(true));
        let swapper = {
            let keep_swapping = Arc::clone(&keep_swapping);
            let dir = root.dir.clone();
            std::thread::spawn(move || {
                let regular_spare = dir.join("regular-spare");
                let fifo_spare = dir.join("fifo-spare");
                let target = dir.join(target_name);
                while keep_swapping.load(Ordering::Relaxed) {
                    // `rename` replaces the destination in one step, the
                    // same atomic replacement `docs/protocol.md` section 8
                    // relies on for a real transfer landing its final file.
                    let _ = std::fs::write(&regular_spare, b"hello");
                    let _ = std::fs::rename(&regular_spare, &target);
                    let made = std::process::Command::new("mkfifo")
                        .arg(&fifo_spare)
                        .status()
                        .is_ok_and(|status| status.success());
                    if made {
                        let _ = std::fs::rename(&fifo_spare, &target);
                    }
                }
            })
        };

        let fs = root.fs();
        let (done_send, done_recv) = mpsc::channel();
        std::thread::spawn(move || {
            let target = path(target_name);
            for _ in 0..5_000u32 {
                match fs.read(&target, 0, 5) {
                    Ok(_) | Err(OpError::Unsupported) => {}
                    Err(other) => {
                        let _ = done_send
                            .send(Err(format!("expected bytes or Unsupported, got {other:?}")));
                        return;
                    }
                }
            }
            let _ = done_send.send(Ok(()));
        });

        // A blocked read would hang the reading thread forever, not just for
        // ten seconds, so this deadline is what turns that hang into an
        // observable test failure instead of a stuck test binary. The
        // reading thread itself is abandoned if the deadline is hit; nothing
        // needs to join it, because a process exit tears down every thread,
        // blocked or not.
        let outcome = done_recv.recv_timeout(Duration::from_secs(10));
        keep_swapping.store(false, Ordering::Relaxed);
        swapper.join().unwrap();

        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(message)) => panic!("{message}"),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "5000 reads did not finish within the 10 second deadline; \
                 a read most likely blocked on an open FIFO"
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the reading thread ended without reporting a result")
            }
        }
    }

    #[test]
    fn paging_a_large_directory_sorts_it_once() {
        let root = TempRoot::new("list-cache-timing");
        let fs = root.fs();
        fs.mkdir(&path("many")).unwrap();

        let total = 3_000usize;
        for i in 0..total {
            fs.write(&path(&format!("many/{i:05}")), 0, b"").unwrap();
        }

        // FINDING 4: `list` used to read and sort the whole directory again
        // on every page, which measured at 118 ms a page at 60,000 entries.
        // The first page still pays for one read and one sort; every later
        // page in the same paging run must not.
        let first_start = Instant::now();
        let (first_page, mut cursor) = fs.list(&path("many"), 0).unwrap();
        let first_page_time = first_start.elapsed();
        assert_eq!(first_page.len(), limits::MAX_LIST_ENTRIES as usize);

        let mut seen = first_page.len();
        let mut second_page_time = None;
        while let Some(next) = cursor {
            let page_start = Instant::now();
            let (page, next_cursor) = fs.list(&path("many"), next).unwrap();
            second_page_time.get_or_insert_with(|| page_start.elapsed());
            seen += page.len();
            cursor = next_cursor;
        }
        let total_time = first_start.elapsed();

        assert_eq!(seen, total);
        // A generous bound. Reading, sorting, and paging 3000 entries should
        // not come close to this on any machine that can run the test suite
        // at all; it is here to catch a real regression, not to be tight.
        assert!(
            total_time < Duration::from_secs(5),
            "paging 3000 entries took {total_time:?}"
        );

        let second_page_time = second_page_time
            .expect("3000 entries at a 1024 entry page size must page more than once");
        assert!(
            second_page_time < first_page_time / 4,
            "the second page ({second_page_time:?}) should be well under a \
             quarter of the first page's time ({first_page_time:?}); the \
             cache should mean it skips reading and sorting the directory \
             again"
        );
    }
}
