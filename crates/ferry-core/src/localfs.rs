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
//!    [`crate::ops::OpError::Unsupported`].
//! 3. `delete` is not recursive. A directory holding anything returns
//!    [`crate::ops::OpError::NotEmpty`].
//! 4. `rename` replaces the destination in one step.
//! 5. `read` past the end returns fewer bytes. `write` past the end extends
//!    with zeros. `truncate` shortens or extends.
//! 6. `list` pages at [`crate::limits::MAX_LIST_ENTRIES`], sorted by name, with
//!    the cursor as a zero-based index into the sorted children.
//! 7. Every std or `cap_std` error maps to one [`crate::ops::OpError`] variant.
//!    The mapping lives in one function.
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

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, Metadata, OpenOptions};

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
        Ok(Self { root })
    }
}

impl FileOps for LocalFs {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        // `symlink_metadata` never follows the last component, so a symlink
        // left at `path` is classified as `Unsupported`, not as whatever it
        // points to.
        let dir_metadata = self
            .root
            .symlink_metadata(path.as_str())
            .map_err(|e| map_io(&e))?;
        match classify(&dir_metadata)? {
            FileKind::Directory => {}
            FileKind::File => return Err(OpError::NotADirectory),
        }

        let mut children = Vec::new();
        for entry in self.root.read_dir(path.as_str()).map_err(|e| map_io(&e))? {
            let entry = entry.map_err(|e| map_io(&e))?;
            // One odd child must not fail the whole listing. A FIFO, a
            // socket, a device, or a symlink is left out instead.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(kind) = classify(&metadata) else {
                continue;
            };
            children.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
                size: if kind == FileKind::File {
                    metadata.len()
                } else {
                    0
                },
                modified_unix_secs: modified_secs(&metadata),
            });
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));

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
        Ok((children[start..end].to_vec(), next_cursor))
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        let metadata = self
            .root
            .symlink_metadata(path.as_str())
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

        // The type is checked with a `stat`-only call before anything is
        // opened. A FIFO would block the calling thread forever if it were
        // opened here, and a symlink would be silently followed.
        let metadata = self
            .root
            .symlink_metadata(path.as_str())
            .map_err(|e| map_io(&e))?;
        match classify(&metadata)? {
            FileKind::File => {}
            FileKind::Directory => return Err(OpError::IsADirectory),
        }

        let mut file = self.root.open(path.as_str()).map_err(|e| map_io(&e))?;
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

        // A missing path is fine: `write` creates the file. Anything already
        // there must be checked before it is opened, for the same reason as
        // in `read`. A failure other than "missing" here is not turned into
        // an `OpError`; the open call below hits the same failure and maps
        // it through `map_io`.
        if let Ok(metadata) = self.root.symlink_metadata(path.as_str()) {
            match classify(&metadata)? {
                FileKind::File => {}
                FileKind::Directory => return Err(OpError::IsADirectory),
            }
        }

        let mut file = self
            .root
            .open_with(path.as_str(), OpenOptions::new().write(true).create(true))
            .map_err(|e| map_io(&e))?;
        file.seek(SeekFrom::Start(offset)).map_err(|e| map_io(&e))?;
        file.write_all(bytes).map_err(|e| map_io(&e))?;
        Ok(written)
    }

    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError> {
        let metadata = self
            .root
            .symlink_metadata(path.as_str())
            .map_err(|e| map_io(&e))?;
        match classify(&metadata)? {
            FileKind::File => {}
            FileKind::Directory => return Err(OpError::IsADirectory),
        }
        let file = self
            .root
            .open_with(path.as_str(), OpenOptions::new().write(true))
            .map_err(|e| map_io(&e))?;
        file.set_len(length).map_err(|e| map_io(&e))
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError> {
        let metadata = self
            .root
            .symlink_metadata(from.as_str())
            .map_err(|e| map_io(&e))?;
        classify(&metadata)?;
        self.root
            .rename(from.as_str(), &self.root, to.as_str())
            .map_err(|e| map_io(&e))
    }

    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError> {
        let metadata = self
            .root
            .symlink_metadata(path.as_str())
            .map_err(|e| map_io(&e))?;
        classify(&metadata)?;
        // A plain read-only open is enough. Setting a file's times depends on
        // ownership and permission, not on how the handle was opened.
        let file = self.root.open(path.as_str()).map_err(|e| map_io(&e))?;
        file.into_std()
            .set_modified(unix_secs_to_system_time(modified_unix_secs))
            .map_err(|e| map_io(&e))
    }

    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError> {
        self.root.create_dir(path.as_str()).map_err(|e| map_io(&e))
    }

    fn delete(&self, path: &RemotePath) -> Result<(), OpError> {
        let metadata = self
            .root
            .symlink_metadata(path.as_str())
            .map_err(|e| map_io(&e))?;
        match classify(&metadata)? {
            FileKind::File => self.root.remove_file(path.as_str()).map_err(|e| map_io(&e)),
            // `remove_dir` is a plain `rmdir`. It fails with `NotEmpty`
            // instead of taking anything down with it, because delete is not
            // recursive.
            FileKind::Directory => self.root.remove_dir(path.as_str()).map_err(|e| map_io(&e)),
        }
    }
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
// component is read directly off the string instead. A `RemotePath` always
// has at least one component, so the fallback is never actually used; it
// only keeps this function free of an `unwrap`.
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::LocalFs;
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
        // Reading a FIFO with no writer would block forever if `read` ever
        // opened it. It must be refused before that, from the metadata alone.
        assert_eq!(fs.read(&path("sub/pipe"), 0, 10), Err(OpError::Unsupported));

        let (entries, next_cursor) = fs.list(&path("sub"), 0).unwrap();
        assert_eq!(next_cursor, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt"]);
    }
}
