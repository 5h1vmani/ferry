//! An in-memory [`FileOps`] implementation.
//!
//! [`MemoryFs`] is the reference filesystem. It exists so the protocol can be
//! tested without touching a disk, and so the behaviour every real backend
//! must match has one clear description in code. It is compiled in every
//! build, not only under `cfg(test)`, so any crate can use it as a test
//! double.
//!
//! Every path is stored under its full string, from [`RemotePath::as_str`].
//! A directory does not hold a list of its children. Children are found by
//! scanning for keys whose parent matches, which keeps the map simple at the
//! cost of an `O(n)` scan on `list` and `delete`. That trade is fine here,
//! because this type only needs to be easy to read, not fast.
//!
//! The empty root path has no entry in the map, because [`RemotePath`] cannot
//! represent it. A path with no `/` is a top-level entry, and its parent is
//! the root, which always exists.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::limits;
use crate::ops::{Entry, FileKind, OpError};
use crate::path::RemotePath;
use crate::rpc::FileOps;

/// One thing the map can hold at a path.
///
/// A directory carries no children of its own; see the module documentation
/// for why. It still needs a modification time, because `stat` and
/// `set_mtime` both apply to directories as well as files.
#[derive(Debug)]
enum Node {
    /// A regular file.
    File {
        /// The file's contents.
        bytes: Vec<u8>,
        /// The last modification time, in seconds since the Unix epoch.
        modified_unix_secs: i64,
    },
    /// A directory.
    Directory {
        /// The last modification time, in seconds since the Unix epoch.
        modified_unix_secs: i64,
    },
}

/// A filesystem held entirely in memory.
///
/// This is the reference implementation of [`FileOps`]. Tests build a tree
/// with [`MemoryFs::insert_file`] and [`MemoryFs::insert_dir`], then serve it
/// over a real connection.
#[derive(Debug)]
pub struct MemoryFs {
    nodes: Mutex<BTreeMap<String, Node>>,
}

impl MemoryFs {
    /// Start an empty filesystem, with nothing but the root.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Mutex::new(BTreeMap::new()),
        }
    }

    /// Create a file at `path`, and any missing parent directories.
    ///
    /// This is a test helper. It builds a tree in one call, instead of one
    /// `mkdir` per level plus a `write`. It overwrites whatever was at
    /// `path` before.
    pub fn insert_file(&self, path: &str, bytes: Vec<u8>) {
        let mut nodes = self.lock();
        ensure_parents(&mut nodes, path);
        nodes.insert(
            path.to_string(),
            Node::File {
                bytes,
                modified_unix_secs: 0,
            },
        );
    }

    /// Create a directory at `path`, and any missing parent directories.
    ///
    /// This is a test helper, for the same reason as [`MemoryFs::insert_file`].
    pub fn insert_dir(&self, path: &str) {
        let mut nodes = self.lock();
        ensure_parents(&mut nodes, path);
        nodes.insert(
            path.to_string(),
            Node::Directory {
                modified_unix_secs: 0,
            },
        );
    }

    /// The bytes stored at `path`, or `None` when it is not a file.
    ///
    /// This is a test helper, so a test can check what a `write` landed
    /// without going through `read`.
    #[must_use]
    pub fn file_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let nodes = self.lock();
        match nodes.get(path) {
            Some(Node::File { bytes, .. }) => Some(bytes.clone()),
            _ => None,
        }
    }

    // A poisoned lock still holds a valid map; the panic that poisoned it
    // happened in some other call, not in the data itself. Recovering keeps
    // one bad request from wedging every later one.
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Node>> {
        self.nodes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for MemoryFs {
    fn default() -> Self {
        Self::new()
    }
}

// Builds every ancestor directory of `path` that is not already present.
// The leaf itself is left for the caller to insert, since the caller knows
// whether it should be a file or a directory.
fn ensure_parents(nodes: &mut BTreeMap<String, Node>, path: &str) {
    let mut built = String::new();
    for part in path.split('/') {
        if !built.is_empty() {
            built.push('/');
        }
        built.push_str(part);
        if built == path {
            break;
        }
        nodes
            .entry(built.clone())
            .or_insert_with(|| Node::Directory {
                modified_unix_secs: 0,
            });
    }
}

// The parent path of `key`, or `None` when `key` is a top-level entry. A
// top-level entry's parent is the root, which is never a key in the map,
// because `RemotePath` cannot represent an empty path.
fn parent_of(key: &str) -> Option<&str> {
    key.rsplit_once('/').map(|(parent, _)| parent)
}

// The last path component of `key`.
fn name_of(key: &str) -> &str {
    key.rsplit_once('/').map_or(key, |(_, name)| name)
}

// Builds the `Entry` a `list` or `stat` call returns for one node.
fn entry_for(name: &str, node: &Node) -> Entry {
    match node {
        Node::File {
            bytes,
            modified_unix_secs,
        } => Entry {
            name: name.to_string(),
            kind: FileKind::File,
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            modified_unix_secs: *modified_unix_secs,
        },
        Node::Directory { modified_unix_secs } => Entry {
            name: name.to_string(),
            kind: FileKind::Directory,
            size: 0,
            modified_unix_secs: *modified_unix_secs,
        },
    }
}

impl FileOps for MemoryFs {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        let nodes = self.lock();
        match nodes.get(path.as_str()) {
            None => Err(OpError::NotFound),
            Some(Node::File { .. }) => Err(OpError::NotADirectory),
            Some(Node::Directory { .. }) => {
                // Every child's key starts with the parent's key, so a
                // `BTreeMap` already yields them in name order. No separate
                // sort is needed.
                let children: Vec<(&str, &Node)> = nodes
                    .iter()
                    .filter(|(key, _)| parent_of(key) == Some(path.as_str()))
                    .map(|(key, node)| (name_of(key), node))
                    .collect();

                let page = usize::try_from(limits::MAX_LIST_ENTRIES).unwrap_or(usize::MAX);
                let start = usize::try_from(cursor)
                    .unwrap_or(usize::MAX)
                    .min(children.len());
                let end = start.saturating_add(page).min(children.len());

                let entries = children[start..end]
                    .iter()
                    .map(|(name, node)| entry_for(name, node))
                    .collect();
                let next_cursor = if end < children.len() {
                    Some(u64::try_from(end).unwrap_or(u64::MAX))
                } else {
                    None
                };
                Ok((entries, next_cursor))
            }
        }
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        let nodes = self.lock();
        let node = nodes.get(path.as_str()).ok_or(OpError::NotFound)?;
        Ok(entry_for(name_of(path.as_str()), node))
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if length > limits::MAX_READ_LEN {
            return Err(OpError::RangeTooLarge);
        }
        let nodes = self.lock();
        match nodes.get(path.as_str()) {
            None => Err(OpError::NotFound),
            Some(Node::Directory { .. }) => Err(OpError::IsADirectory),
            Some(Node::File { bytes, .. }) => {
                let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                if offset >= bytes.len() {
                    return Ok(Vec::new());
                }
                let length = usize::try_from(length).unwrap_or(usize::MAX);
                let end = offset.saturating_add(length).min(bytes.len());
                Ok(bytes[offset..end].to_vec())
            }
        }
    }

    fn write(&self, path: &RemotePath, offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        let written = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        if written > limits::MAX_WRITE_LEN {
            return Err(OpError::RangeTooLarge);
        }
        let mut nodes = self.lock();
        let node = nodes
            .entry(path.as_str().to_string())
            .or_insert_with(|| Node::File {
                bytes: Vec::new(),
                modified_unix_secs: 0,
            });
        let file_bytes = match node {
            Node::Directory { .. } => return Err(OpError::IsADirectory),
            Node::File { bytes, .. } => bytes,
        };

        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset > file_bytes.len() {
            file_bytes.resize(offset, 0);
        }
        let end = offset.saturating_add(bytes.len());
        if end > file_bytes.len() {
            file_bytes.resize(end, 0);
        }
        file_bytes[offset..end].copy_from_slice(bytes);
        Ok(written)
    }

    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError> {
        let mut nodes = self.lock();
        match nodes.get_mut(path.as_str()) {
            None => Err(OpError::NotFound),
            Some(Node::Directory { .. }) => Err(OpError::IsADirectory),
            Some(Node::File { bytes, .. }) => {
                let length = usize::try_from(length).unwrap_or(usize::MAX);
                bytes.resize(length, 0);
                Ok(())
            }
        }
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError> {
        let mut nodes = self.lock();
        if !nodes.contains_key(from.as_str()) {
            return Err(OpError::NotFound);
        }

        // A directory carries its descendants along by their key prefix, so
        // a rename of a directory moves everything under it in one pass.
        let from_prefix = format!("{}/", from.as_str());
        let to_prefix = format!("{}/", to.as_str());

        let moving: Vec<String> = nodes
            .keys()
            .filter(|key| key.as_str() == from.as_str() || key.starts_with(&from_prefix))
            .cloned()
            .collect();

        // The destination is replaced in one step, so anything already
        // there, file or directory tree, is dropped before the move lands.
        let replaced: Vec<String> = nodes
            .keys()
            .filter(|key| key.as_str() == to.as_str() || key.starts_with(&to_prefix))
            .cloned()
            .collect();
        for key in replaced {
            nodes.remove(&key);
        }

        for key in moving {
            if let Some(node) = nodes.remove(&key) {
                let new_key = if key == from.as_str() {
                    to.as_str().to_string()
                } else {
                    format!("{to_prefix}{}", &key[from_prefix.len()..])
                };
                nodes.insert(new_key, node);
            }
        }
        Ok(())
    }

    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError> {
        let mut nodes = self.lock();
        match nodes.get_mut(path.as_str()) {
            None => Err(OpError::NotFound),
            Some(
                Node::File {
                    modified_unix_secs: stored,
                    ..
                }
                | Node::Directory {
                    modified_unix_secs: stored,
                },
            ) => {
                *stored = modified_unix_secs;
                Ok(())
            }
        }
    }

    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError> {
        let mut nodes = self.lock();
        if nodes.contains_key(path.as_str()) {
            return Err(OpError::AlreadyExists);
        }
        // A parent is either the root, which always exists, or a directory
        // that must already be present. `mkdir` never creates parents.
        if let Some(parent) = parent_of(path.as_str())
            && !matches!(nodes.get(parent), Some(Node::Directory { .. }))
        {
            return Err(OpError::NotFound);
        }
        nodes.insert(
            path.as_str().to_string(),
            Node::Directory {
                modified_unix_secs: 0,
            },
        );
        Ok(())
    }

    fn delete(&self, path: &RemotePath) -> Result<(), OpError> {
        let mut nodes = self.lock();
        match nodes.get(path.as_str()) {
            None => Err(OpError::NotFound),
            Some(Node::Directory { .. }) => {
                let prefix = format!("{}/", path.as_str());
                if nodes.keys().any(|key| key.starts_with(&prefix)) {
                    return Err(OpError::NotEmpty);
                }
                nodes.remove(path.as_str());
                Ok(())
            }
            Some(Node::File { .. }) => {
                nodes.remove(path.as_str());
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryFs;
    use crate::limits;
    use crate::ops::{FileKind, OpError};
    use crate::path::RemotePath;
    use crate::rpc::FileOps;

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    #[test]
    fn list_returns_direct_children_only_sorted_by_name() {
        let fs = MemoryFs::new();
        fs.insert_file("DCIM/b.jpg", b"b".to_vec());
        fs.insert_file("DCIM/a.jpg", b"a".to_vec());
        fs.insert_dir("DCIM/Sub");
        fs.insert_file("DCIM/Sub/deep.jpg", b"deep".to_vec());

        let (entries, next_cursor) = fs.list(&path("DCIM"), 0).unwrap();
        assert_eq!(next_cursor, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Sub", "a.jpg", "b.jpg"]);
    }

    #[test]
    fn list_pages_at_the_entry_limit_using_the_cursor_as_an_index() {
        let fs = MemoryFs::new();
        let total = limits::MAX_LIST_ENTRIES as usize + 5;
        for i in 0..total {
            fs.insert_file(&format!("D/{i:05}"), Vec::new());
        }

        let (first_page, next_cursor) = fs.list(&path("D"), 0).unwrap();
        assert_eq!(first_page.len(), limits::MAX_LIST_ENTRIES as usize);
        let next_cursor = next_cursor.expect("more entries remain");

        let (second_page, next_cursor) = fs.list(&path("D"), next_cursor).unwrap();
        assert_eq!(second_page.len(), 5);
        assert_eq!(next_cursor, None);
    }

    #[test]
    fn list_on_a_file_is_not_a_directory() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"a".to_vec());
        assert_eq!(fs.list(&path("a.jpg"), 0), Err(OpError::NotADirectory));
    }

    #[test]
    fn list_on_a_missing_path_is_not_found() {
        let fs = MemoryFs::new();
        assert_eq!(fs.list(&path("nope"), 0), Err(OpError::NotFound));
    }

    #[test]
    fn stat_on_a_missing_path_is_not_found() {
        let fs = MemoryFs::new();
        assert_eq!(fs.stat(&path("nope")), Err(OpError::NotFound));
    }

    #[test]
    fn stat_describes_a_file() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"hello".to_vec());
        fs.set_mtime(&path("a.jpg"), 42).unwrap();
        let entry = fs.stat(&path("a.jpg")).unwrap();
        assert_eq!(entry.name, "a.jpg");
        assert_eq!(entry.kind, FileKind::File);
        assert_eq!(entry.size, 5);
        assert_eq!(entry.modified_unix_secs, 42);
    }

    #[test]
    fn read_past_the_end_returns_fewer_bytes() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"hello".to_vec());
        let bytes = fs.read(&path("a.jpg"), 3, 10).unwrap();
        assert_eq!(bytes, b"lo");
    }

    #[test]
    fn read_entirely_past_the_end_returns_empty() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"hello".to_vec());
        let bytes = fs.read(&path("a.jpg"), 50, 10).unwrap();
        assert_eq!(bytes, Vec::<u8>::new());
    }

    #[test]
    fn read_of_a_directory_is_is_a_directory() {
        let fs = MemoryFs::new();
        fs.insert_dir("DCIM");
        assert_eq!(fs.read(&path("DCIM"), 0, 10), Err(OpError::IsADirectory));
    }

    #[test]
    fn read_over_the_length_limit_is_range_too_large() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"hello".to_vec());
        assert_eq!(
            fs.read(&path("a.jpg"), 0, limits::MAX_READ_LEN + 1),
            Err(OpError::RangeTooLarge)
        );
    }

    #[test]
    fn write_creates_a_missing_file() {
        let fs = MemoryFs::new();
        let written = fs.write(&path("new.txt"), 0, b"hi").unwrap();
        assert_eq!(written, 2);
        assert_eq!(fs.file_bytes("new.txt"), Some(b"hi".to_vec()));
    }

    #[test]
    fn write_past_the_end_extends_with_zeros() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"hi".to_vec());
        fs.write(&path("a.txt"), 5, b"z").unwrap();
        assert_eq!(fs.file_bytes("a.txt"), Some(b"hi\0\0\0z".to_vec()));
    }

    #[test]
    fn write_to_a_directory_is_is_a_directory() {
        let fs = MemoryFs::new();
        fs.insert_dir("DCIM");
        assert_eq!(
            fs.write(&path("DCIM"), 0, b"hi"),
            Err(OpError::IsADirectory)
        );
    }

    #[test]
    fn write_over_the_length_limit_is_range_too_large() {
        let fs = MemoryFs::new();
        let big = vec![0u8; (limits::MAX_WRITE_LEN + 1) as usize];
        assert_eq!(
            fs.write(&path("a.txt"), 0, &big),
            Err(OpError::RangeTooLarge)
        );
    }

    #[test]
    fn truncate_shortens_a_file() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"hello".to_vec());
        fs.truncate(&path("a.txt"), 2).unwrap();
        assert_eq!(fs.file_bytes("a.txt"), Some(b"he".to_vec()));
    }

    #[test]
    fn truncate_extends_a_file_with_zeros() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"hi".to_vec());
        fs.truncate(&path("a.txt"), 4).unwrap();
        assert_eq!(fs.file_bytes("a.txt"), Some(b"hi\0\0".to_vec()));
    }

    #[test]
    fn truncate_on_a_directory_is_is_a_directory() {
        let fs = MemoryFs::new();
        fs.insert_dir("DCIM");
        assert_eq!(fs.truncate(&path("DCIM"), 0), Err(OpError::IsADirectory));
    }

    #[test]
    fn truncate_on_a_missing_path_is_not_found() {
        let fs = MemoryFs::new();
        assert_eq!(fs.truncate(&path("nope"), 0), Err(OpError::NotFound));
    }

    #[test]
    fn rename_moves_a_file_and_replaces_the_destination() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"one".to_vec());
        fs.insert_file("b.txt", b"two".to_vec());
        fs.rename(&path("a.txt"), &path("b.txt")).unwrap();
        assert_eq!(fs.file_bytes("b.txt"), Some(b"one".to_vec()));
        assert_eq!(fs.stat(&path("a.txt")), Err(OpError::NotFound));
    }

    #[test]
    fn rename_moves_an_empty_directory() {
        let fs = MemoryFs::new();
        fs.insert_dir("Old");
        fs.rename(&path("Old"), &path("New")).unwrap();
        assert_eq!(fs.stat(&path("New")).unwrap().kind, FileKind::Directory);
        assert_eq!(fs.stat(&path("Old")), Err(OpError::NotFound));
    }

    #[test]
    fn rename_of_a_missing_source_is_not_found() {
        let fs = MemoryFs::new();
        assert_eq!(
            fs.rename(&path("nope"), &path("also-nope")),
            Err(OpError::NotFound)
        );
    }

    #[test]
    fn set_mtime_on_a_missing_path_is_not_found() {
        let fs = MemoryFs::new();
        assert_eq!(fs.set_mtime(&path("nope"), 1), Err(OpError::NotFound));
    }

    #[test]
    fn mkdir_on_an_existing_path_already_exists() {
        let fs = MemoryFs::new();
        fs.insert_dir("DCIM");
        assert_eq!(fs.mkdir(&path("DCIM")), Err(OpError::AlreadyExists));
    }

    #[test]
    fn mkdir_does_not_create_missing_parents() {
        let fs = MemoryFs::new();
        assert_eq!(fs.mkdir(&path("DCIM/Sub")), Err(OpError::NotFound));
    }

    #[test]
    fn mkdir_at_the_top_level_needs_no_parent() {
        let fs = MemoryFs::new();
        fs.mkdir(&path("DCIM")).unwrap();
        assert_eq!(fs.stat(&path("DCIM")).unwrap().kind, FileKind::Directory);
    }

    #[test]
    fn delete_of_a_directory_holding_anything_is_not_empty() {
        let fs = MemoryFs::new();
        fs.insert_file("DCIM/a.jpg", b"a".to_vec());
        assert_eq!(fs.delete(&path("DCIM")), Err(OpError::NotEmpty));
    }

    #[test]
    fn delete_of_an_empty_directory_succeeds() {
        let fs = MemoryFs::new();
        fs.insert_dir("Empty");
        fs.delete(&path("Empty")).unwrap();
        assert_eq!(fs.stat(&path("Empty")), Err(OpError::NotFound));
    }

    #[test]
    fn delete_of_a_missing_path_is_not_found() {
        let fs = MemoryFs::new();
        assert_eq!(fs.delete(&path("nope")), Err(OpError::NotFound));
    }
}
