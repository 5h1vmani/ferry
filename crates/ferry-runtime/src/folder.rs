//! Listing a folder recursively, over a live connection or over a plain
//! [`FileOps`] filesystem such as [`ferry_core::memfs::MemoryFs`].
//!
//! `docs/engine-contract.md`, batch D, item 2: `pull_folder` lists a whole
//! folder before it queues anything, the same way `Engine::list` lists one
//! page. [`list_recursive`] is that walk, written over [`FileOps`] rather
//! than over the RPC `Client` directly, so its two bounds can be proven
//! against an in-memory filesystem, with no network. [`RemoteLister`] is the
//! seam that lets the very same walk run over a real connection: it adapts a
//! `Client` to [`FileOps`], translating the one call the walk makes.

use std::io::{Read, Write};
use std::sync::Mutex;

use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, RpcError};

use crate::state::lock;

/// The most files one `pull_folder` call may queue. Exactly this many is
/// within bounds; one more is not.
pub(crate) const MAX_FOLDER_FILES: usize = 10_000;

/// The deepest a folder may nest before `pull_folder` refuses it. The
/// folder passed to `pull_folder` is itself depth 1, so nesting exactly
/// this many folders is within bounds and one more is not.
pub(crate) const MAX_FOLDER_DEPTH: u32 = 32;

/// Why [`list_recursive`] stopped before it finished.
#[derive(Debug)]
pub(crate) enum ListRecursiveError {
    /// More than [`MAX_FOLDER_FILES`] files, or nested past
    /// [`MAX_FOLDER_DEPTH`] levels.
    TooLarge,
    /// A call to [`FileOps::list`] itself failed.
    Op(OpError),
}

/// List `root` and every folder under it, and return every file found, as a
/// path from `fs`'s own root paired with its size, in the order the walk
/// visited them.
///
/// Each folder is paged with `fs.list`'s own cursor, the same way
/// `Engine::list` pages one folder. A subfolder is walked as soon as it is
/// seen, before the folder that holds it finishes paging. The size travels
/// with the path so `pull_folder` can log its access log entry without a
/// second round trip (docs/engine-contract.md, item 13).
///
/// # Errors
///
/// Returns [`ListRecursiveError::TooLarge`] at more than [`MAX_FOLDER_FILES`]
/// files or past [`MAX_FOLDER_DEPTH`] levels of nesting, and
/// [`ListRecursiveError::Op`] when a page fails to list.
pub(crate) fn list_recursive(
    fs: &dyn FileOps,
    root: &RemotePath,
) -> Result<Vec<(RemotePath, u64)>, ListRecursiveError> {
    let mut out = Vec::new();
    walk(fs, root, 1, &mut out)?;
    Ok(out)
}

fn walk(
    fs: &dyn FileOps,
    dir: &RemotePath,
    depth: u32,
    out: &mut Vec<(RemotePath, u64)>,
) -> Result<(), ListRecursiveError> {
    if depth > MAX_FOLDER_DEPTH {
        return Err(ListRecursiveError::TooLarge);
    }
    let mut cursor = 0u64;
    loop {
        let (entries, next) = fs.list(dir, cursor).map_err(ListRecursiveError::Op)?;
        for entry in entries {
            let child = join(dir, &entry.name)?;
            match entry.kind {
                FileKind::File => {
                    out.push((child, entry.size));
                    if out.len() > MAX_FOLDER_FILES {
                        return Err(ListRecursiveError::TooLarge);
                    }
                }
                FileKind::Directory => walk(fs, &child, depth + 1, out)?,
            }
        }
        match next {
            Some(next_cursor) => cursor = next_cursor,
            None => break,
        }
    }
    Ok(())
}

/// `dir` joined with one more path segment.
fn join(dir: &RemotePath, name: &str) -> Result<RemotePath, ListRecursiveError> {
    let text = if dir.is_root() {
        name.to_owned()
    } else {
        format!("{}/{name}", dir.as_str())
    };
    RemotePath::parse(&text).map_err(|_| ListRecursiveError::Op(OpError::InvalidPath))
}

/// Adapts a live connection to [`FileOps`], so [`list_recursive`] can walk
/// it the same way it walks an in-memory filesystem in tests.
///
/// [`FileOps`] asks for `Send + Sync`, and [`FileOps::list`] takes `&self`.
/// `Client::list` takes `&mut self`, because it counts request identifiers.
/// The [`Mutex`] supplies both: the interior mutability `FileOps` needs and
/// the `Sync` bound it asks for. Nothing here is ever touched by two
/// threads at once. `list_recursive` only ever calls `list`, so every other
/// method here answers [`OpError::Unsupported`] rather than being reachable
/// in practice.
pub(crate) struct RemoteLister<'a, S: Read + Write + Send> {
    client: Mutex<&'a mut Client<S>>,
    /// The lister only ever calls `list`, and [`FileOps::list`] cannot carry
    /// an [`RpcError`]. A failure that is not the peer refusing the path is
    /// kept here, so the caller can report the real cause instead of a
    /// generic one.
    failure: Mutex<Option<RpcError>>,
}

impl<'a, S: Read + Write + Send> RemoteLister<'a, S> {
    pub(crate) fn new(client: &'a mut Client<S>) -> Self {
        Self {
            client: Mutex::new(client),
            failure: Mutex::new(None),
        }
    }

    /// The connection failure this lister hid behind [`OpError::Internal`],
    /// if `list_recursive` ever hit one. Takes it, so it is reported once.
    pub(crate) fn take_failure(&self) -> Option<RpcError> {
        lock(&self.failure).take()
    }
}

impl<S: Read + Write + Send> FileOps for RemoteLister<'_, S> {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        match lock(&self.client).list(path, cursor) {
            Ok(page) => Ok(page),
            Err(RpcError::Remote(op)) => Err(op),
            Err(other) => {
                *lock(&self.failure) = Some(other);
                Err(OpError::Internal)
            }
        }
    }

    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
    }

    fn write(&self, _path: &RemotePath, _offset: u64, _bytes: &[u8]) -> Result<u32, OpError> {
        Err(OpError::Unsupported)
    }

    fn truncate(&self, _path: &RemotePath, _length: u64) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn rename(&self, _from: &RemotePath, _to: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn set_mtime(&self, _path: &RemotePath, _modified_unix_secs: i64) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn mkdir(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn delete(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::{ListRecursiveError, MAX_FOLDER_DEPTH, MAX_FOLDER_FILES, list_recursive};
    use ferry_core::memfs::MemoryFs;
    use ferry_core::path::RemotePath;

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).expect("a valid path")
    }

    #[test]
    fn a_folder_within_bounds_lists_every_file_with_its_relative_path() {
        let fs = MemoryFs::new();
        fs.insert_file("Camera/a.jpg", b"a".to_vec());
        fs.insert_file("Camera/b.jpg", b"b".to_vec());
        fs.insert_dir("Camera/sub");
        fs.insert_file("Camera/sub/c.jpg", b"c".to_vec());

        let files = list_recursive(&fs, &path("Camera")).expect("within bounds");
        let names: Vec<String> = files.iter().map(|(p, _)| p.as_str().to_owned()).collect();
        assert_eq!(
            names,
            vec![
                "Camera/a.jpg".to_owned(),
                "Camera/b.jpg".to_owned(),
                "Camera/sub/c.jpg".to_owned(),
            ],
            "every file is found, each with its full path from the fs root"
        );
        let sizes: Vec<u64> = files.iter().map(|(_, size)| *size).collect();
        assert_eq!(
            sizes,
            vec![1, 1, 1],
            "each file's size travels with its path"
        );
    }

    #[test]
    fn an_empty_folder_lists_no_files() {
        let fs = MemoryFs::new();
        fs.insert_dir("Camera");
        let files = list_recursive(&fs, &path("Camera")).expect("an empty folder is within bounds");
        assert!(files.is_empty());
    }

    #[test]
    fn exactly_the_file_cap_is_within_bounds() {
        let fs = MemoryFs::new();
        for i in 0..MAX_FOLDER_FILES {
            fs.insert_file(&format!("Camera/{i:05}.jpg"), Vec::new());
        }
        let files = list_recursive(&fs, &path("Camera")).expect("exactly at the cap");
        assert_eq!(files.len(), MAX_FOLDER_FILES);
    }

    #[test]
    fn one_file_over_the_cap_is_too_large() {
        let fs = MemoryFs::new();
        for i in 0..=MAX_FOLDER_FILES {
            fs.insert_file(&format!("Camera/{i:05}.jpg"), Vec::new());
        }
        let error = list_recursive(&fs, &path("Camera")).expect_err("one over the cap");
        assert!(matches!(error, ListRecursiveError::TooLarge));
    }

    #[test]
    fn nested_exactly_to_the_depth_cap_is_within_bounds() {
        let fs = MemoryFs::new();
        // The folder passed to `pull_folder` is depth 1, so this many
        // nested subfolders reaches exactly the depth cap.
        let mut path_text = "Camera".to_owned();
        for _ in 1..MAX_FOLDER_DEPTH {
            path_text.push_str("/Sub");
        }
        fs.insert_file(&format!("{path_text}/deep.jpg"), Vec::new());

        let files = list_recursive(&fs, &path("Camera")).expect("exactly at the depth cap");
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn one_folder_deeper_than_the_cap_is_too_large() {
        let fs = MemoryFs::new();
        let mut path_text = "Camera".to_owned();
        for _ in 1..=MAX_FOLDER_DEPTH {
            path_text.push_str("/Sub");
        }
        fs.insert_file(&format!("{path_text}/deep.jpg"), Vec::new());

        let error = list_recursive(&fs, &path("Camera")).expect_err("one folder past the cap");
        assert!(matches!(error, ListRecursiveError::TooLarge));
    }
}
