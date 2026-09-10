//! Several named, shared folders, served as one [`FileOps`].
//!
//! `docs/engine-contract.md`, batch C, item 15, calls this the largest
//! change to the file operations layer since it was designed: a peer no
//! longer sees one shared folder, but a fixed set of named ones, and every
//! path begins with the name of the one it addresses.
//!
//! [`LocalFs`] does not change. [`Roots`] holds one [`LocalFs`] per root and
//! dispatches every call on the first segment of the path. The rest of the
//! path is handed to that root's `LocalFs` unchanged.
//!
//! # Root rules
//!
//! A root's name is 1 to 64 bytes of UTF-8, holds no control character, no
//! `/`, and no `\`, and is not `.` or `..`. Names are unique ignoring case.
//! A root's path must be an existing directory. There is at least one root.
//! No two roots may share a folder or nest one inside another, once
//! symlinks are resolved: a read-only root nested inside a writable one
//! could otherwise be written through the other name.
//!
//! # Protocol behaviour
//!
//! - `list("")` returns one entry per root: its name, size 0, and the
//!   modified time of its folder. One page, no cursor.
//! - `stat("")` is a directory entry.
//! - A path whose first segment names no root is [`OpError::NotFound`].
//! - Any `write`, `truncate`, `mkdir`, `delete`, or `rename` that touches a
//!   root marked not writable is [`OpError::PermissionDenied`].
//! - `mkdir`, `delete`, and `rename` refuse a path of exactly one segment
//!   that names an existing root, with [`OpError::PermissionDenied`]: that
//!   path names the root itself, not something inside it.
//! - `rename` across two different roots is [`OpError::Unsupported`].
//! - `set_mtime` follows the writable rule above. Unlike `mkdir`, `delete`,
//!   and `rename`, it is not refused on a root's own one-segment path: a
//!   root is a real folder, and setting its own folder's time is the same
//!   operation `LocalFs` already allows on its shared root.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::localfs::LocalFs;
use crate::ops::{Entry, FileKind, OpError};
use crate::path::RemotePath;
use crate::rpc::FileOps;

/// One shared folder to give to [`Roots::open`].
#[derive(Debug, Clone)]
pub struct RootSpec {
    /// What a peer sees as this root's first path segment, such as
    /// `"Desktop"`. See the module documentation for the naming rules.
    pub name: String,
    /// Where this root lives on disk. Must be an existing directory.
    pub path: PathBuf,
    /// False for a root a peer may read but not write.
    pub writable: bool,
}

/// The reason [`Roots::open`] refused a set of roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RootsError {
    /// A root's name is empty, over 64 bytes, holds a control character or
    /// a `/`, or is `.` or `..`.
    #[error("a root's name failed validation")]
    RootNameInvalid,
    /// Two roots were given the same name, ignoring case.
    #[error("a root's name is already used by another root")]
    RootNameTaken,
    /// A root's path is not an existing directory.
    #[error("a root's path is not an existing folder")]
    RootNotAFolder,
    /// Two roots resolve to the same folder, or one sits inside the other,
    /// once symlinks are resolved.
    #[error("two roots overlap on disk")]
    RootOverlaps,
    /// `Roots::open` was given no roots at all.
    #[error("there are no roots")]
    NoRoots,
}

// One opened root, keyed by its lowercased name in `Roots::by_name`.
#[derive(Debug)]
struct RootEntry {
    // The name as configured. `by_name`'s key is this, lowercased, so the
    // original casing is kept here for anything Roots builds itself: the
    // union listing at `list("")`, and a `stat` of the root's own segment.
    display_name: String,
    writable: bool,
    fs: LocalFs,
}

/// Several named [`LocalFs`] roots, served through one [`FileOps`].
///
/// See the module documentation for the naming rules and the protocol
/// behaviour this type implements.
#[derive(Debug)]
pub struct Roots {
    by_name: HashMap<String, RootEntry>,
}

impl Roots {
    /// Open every root in `specs` and serve them as one [`FileOps`].
    ///
    /// # Errors
    ///
    /// Returns [`RootsError::NoRoots`] when `specs` is empty,
    /// [`RootsError::RootNameInvalid`] when a name breaks the rules on
    /// [`RootSpec::name`], [`RootsError::RootNameTaken`] when two names
    /// collide ignoring case, [`RootsError::RootNotAFolder`] when a path is
    /// not an existing directory, and [`RootsError::RootOverlaps`] when two
    /// roots resolve to the same folder or one nests inside another.
    pub fn open(specs: Vec<RootSpec>) -> Result<Self, RootsError> {
        if specs.is_empty() {
            return Err(RootsError::NoRoots);
        }
        let mut by_name = HashMap::with_capacity(specs.len());
        // The canonical (symlink-resolved) path of every root opened so
        // far, checked against each new one below. Two different strings
        // can still name the same folder, so only the resolved form is
        // safe to compare.
        let mut canonical_paths: Vec<PathBuf> = Vec::with_capacity(specs.len());
        for spec in specs {
            validate_root_name(&spec.name)?;
            // Unicode case folding, not just ASCII: a name is unique
            // ignoring case, and a root's name is not limited to ASCII.
            let key = spec.name.to_lowercase();
            if by_name.contains_key(&key) {
                return Err(RootsError::RootNameTaken);
            }
            let fs = LocalFs::open(&spec.path).map_err(|_| RootsError::RootNotAFolder)?;

            // Resolve symlinks first, the same real folder `LocalFs::open`
            // just opened above. Comparing the raw configured text would
            // miss two paths that reach the same folder through a
            // symlink. `Path::starts_with` compares whole components, not
            // characters, so `/a/b` does not match `/a/bc`; checking both
            // directions also catches the two paths being equal.
            let canonical =
                std::fs::canonicalize(&spec.path).map_err(|_| RootsError::RootNotAFolder)?;
            for existing in &canonical_paths {
                if canonical.starts_with(existing) || existing.starts_with(&canonical) {
                    return Err(RootsError::RootOverlaps);
                }
            }

            by_name.insert(
                key,
                RootEntry {
                    display_name: spec.name,
                    writable: spec.writable,
                    fs,
                },
            );
            canonical_paths.push(canonical);
        }
        Ok(Self { by_name })
    }

    // One [`Entry`] per root, for `list("")`: the root's name, a directory,
    // size 0, and its folder's real modified time. Sorted by name, so the
    // page is deterministic regardless of the `HashMap`'s own order.
    fn root_entries(&self) -> Result<Vec<Entry>, OpError> {
        let root_of_its_own_fs = RemotePath::parse("").expect("the empty path always parses");
        let mut entries = Vec::with_capacity(self.by_name.len());
        for root in self.by_name.values() {
            let stat = root.fs.stat(&root_of_its_own_fs)?;
            entries.push(Entry {
                name: root.display_name.clone(),
                kind: FileKind::Directory,
                size: 0,
                modified_unix_secs: stat.modified_unix_secs,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    // Splits a non-root path on its first segment and looks up the root it
    // names. The remainder is re-parsed as a `RemotePath` of its own: it is
    // a suffix of a path that already passed `RemotePath::parse`, split on
    // a component boundary, so it is always itself valid. When `path` is a
    // single segment, the remainder is the empty string, which parses as
    // that root's own shared root; callers that must tell that case apart
    // from a path reaching inside the root check `RemotePath::is_root` on
    // what this returns.
    //
    // Never called with `path.is_root()` true: every trait method below
    // checks that first, since there is no single root to dispatch to for
    // the union itself.
    fn locate(&self, path: &RemotePath) -> Result<(&RootEntry, RemotePath), OpError> {
        let (first, rest) = split_first_segment(path);
        let root = self
            .by_name
            .get(&first.to_lowercase())
            .ok_or(OpError::NotFound)?;
        let sub = RemotePath::parse(rest).expect("a validated path's suffix is itself valid");
        Ok((root, sub))
    }
}

// The first segment of a non-root path, and the text after it. Kept apart
// from `Roots::locate` because `rename` needs to compare which root two
// paths name before it can decide whether they name the same one.
fn split_first_segment(path: &RemotePath) -> (&str, &str) {
    let full = path.as_str();
    full.split_once('/').unwrap_or((full, ""))
}

impl FileOps for Roots {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        if path.is_root() {
            // The union always fits one page; whatever cursor was asked
            // for, there is only one page to give back.
            let _ = cursor;
            return Ok((self.root_entries()?, None));
        }
        let (root, sub) = self.locate(path)?;
        root.fs.list(&sub, cursor)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        if path.is_root() {
            return Ok(Entry {
                name: String::new(),
                kind: FileKind::Directory,
                size: 0,
                // No single folder backs the union itself, so there is no
                // real time to report. `LocalFs` uses the same placeholder
                // when a platform cannot report a file's time at all.
                modified_unix_secs: 0,
            });
        }
        let (root, sub) = self.locate(path)?;
        let mut entry = root.fs.stat(&sub)?;
        if sub.is_root() {
            // `path` was one segment: the root itself. `LocalFs::stat` of
            // its own shared root names it with the empty string, since it
            // has no name of its own; here it does, so that name replaces
            // the placeholder.
            entry.name.clone_from(&root.display_name);
        }
        Ok(entry)
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if path.is_root() {
            return Err(OpError::IsADirectory);
        }
        let (root, sub) = self.locate(path)?;
        root.fs.read(&sub, offset, length)
    }

    fn write(&self, path: &RemotePath, offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        if path.is_root() {
            return Err(OpError::IsADirectory);
        }
        let (root, sub) = self.locate(path)?;
        if !root.writable {
            return Err(OpError::PermissionDenied);
        }
        root.fs.write(&sub, offset, bytes)
    }

    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError> {
        if path.is_root() {
            return Err(OpError::IsADirectory);
        }
        let (root, sub) = self.locate(path)?;
        if !root.writable {
            return Err(OpError::PermissionDenied);
        }
        root.fs.truncate(&sub, length)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError> {
        if from.is_root() || to.is_root() {
            return Err(OpError::PermissionDenied);
        }
        let (from_root, from_sub) = self.locate(from)?;
        let (_, to_sub) = self.locate(to)?;
        if from_sub.is_root() || to_sub.is_root() {
            // One side is a single segment: a root itself, not something
            // inside one.
            return Err(OpError::PermissionDenied);
        }
        let (from_name, _) = split_first_segment(from);
        let (to_name, _) = split_first_segment(to);
        if from_name.to_lowercase() != to_name.to_lowercase() {
            return Err(OpError::Unsupported);
        }
        if !from_root.writable {
            return Err(OpError::PermissionDenied);
        }
        from_root.fs.rename(&from_sub, &to_sub)
    }

    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError> {
        if path.is_root() {
            // Nothing backs the union itself, so there is no time to set.
            return Err(OpError::Unsupported);
        }
        let (root, sub) = self.locate(path)?;
        if !root.writable {
            return Err(OpError::PermissionDenied);
        }
        root.fs.set_mtime(&sub, modified_unix_secs)
    }

    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError> {
        if path.is_root() {
            // The union is always already there, the same answer `LocalFs`
            // gives for `mkdir` of its own shared root.
            return Err(OpError::AlreadyExists);
        }
        let (root, sub) = self.locate(path)?;
        if sub.is_root() {
            return Err(OpError::PermissionDenied);
        }
        if !root.writable {
            return Err(OpError::PermissionDenied);
        }
        root.fs.mkdir(&sub)
    }

    fn delete(&self, path: &RemotePath) -> Result<(), OpError> {
        if path.is_root() {
            return Err(OpError::PermissionDenied);
        }
        let (root, sub) = self.locate(path)?;
        if sub.is_root() {
            return Err(OpError::PermissionDenied);
        }
        if !root.writable {
            return Err(OpError::PermissionDenied);
        }
        root.fs.delete(&sub)
    }
}

// A name is 1 to 64 bytes of UTF-8, holds no control character, no `/`, and
// no `\`, and is not `.` or `..`. See `docs/engine-contract.md`, batch C,
// item 15. The backslash is refused for the same reason `RemotePath::parse`
// refuses one in `path.rs`: a root named with one could never be addressed,
// since its name is the path's first segment.
fn validate_root_name(name: &str) -> Result<(), RootsError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(RootsError::RootNameInvalid)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{RootSpec, Roots, RootsError};
    use crate::ops::{FileKind, OpError};
    use crate::path::RemotePath;
    use crate::rpc::FileOps;

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    /// A directory under the system temp directory, unique to one test,
    /// removed when the test ends, including when it panics.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ferry-roots-{label}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn spec(name: &str, dir: &TempDir, writable: bool) -> RootSpec {
        RootSpec {
            name: name.to_string(),
            path: dir.path.clone(),
            writable,
        }
    }

    #[test]
    fn list_of_the_root_shows_every_root_by_name() {
        let a = TempDir::new("list-a");
        let b = TempDir::new("list-b");
        let roots =
            Roots::open(vec![spec("Desktop", &a, true), spec("Downloads", &b, true)]).unwrap();

        let (entries, next_cursor) = roots.list(&path(""), 0).unwrap();
        assert_eq!(next_cursor, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Desktop", "Downloads"]);
        for entry in &entries {
            assert_eq!(entry.kind, FileKind::Directory);
            assert_eq!(entry.size, 0);
        }
    }

    #[test]
    fn stat_of_the_root_is_a_directory_entry() {
        let a = TempDir::new("stat-root");
        let roots = Roots::open(vec![spec("Desktop", &a, true)]).unwrap();
        let entry = roots.stat(&path("")).unwrap();
        assert_eq!(entry.kind, FileKind::Directory);
    }

    #[test]
    fn a_path_into_a_root_reaches_the_file() {
        let dir = TempDir::new("reach");
        std::fs::write(dir.path.join("a.txt"), b"hello").unwrap();
        let roots = Roots::open(vec![spec("Desktop", &dir, true)]).unwrap();

        let bytes = roots.read(&path("Desktop/a.txt"), 0, 5).unwrap();
        assert_eq!(bytes, b"hello");

        let entry = roots.stat(&path("Desktop/a.txt")).unwrap();
        assert_eq!(entry.name, "a.txt");
        assert_eq!(entry.kind, FileKind::File);
    }

    #[test]
    fn an_unknown_first_segment_is_not_found() {
        let dir = TempDir::new("unknown");
        let roots = Roots::open(vec![spec("Desktop", &dir, true)]).unwrap();
        assert_eq!(roots.stat(&path("Nope/a.txt")), Err(OpError::NotFound));
        assert_eq!(roots.list(&path("Nope"), 0), Err(OpError::NotFound));
        assert_eq!(
            roots.read(&path("Nope/a.txt"), 0, 1),
            Err(OpError::NotFound)
        );
    }

    #[test]
    fn a_read_only_root_refuses_a_write_and_allows_a_read() {
        let dir = TempDir::new("readonly");
        std::fs::write(dir.path.join("a.txt"), b"hello").unwrap();
        let roots = Roots::open(vec![spec("Desktop", &dir, false)]).unwrap();

        assert_eq!(
            roots.write(&path("Desktop/a.txt"), 0, b"x"),
            Err(OpError::PermissionDenied)
        );
        assert_eq!(roots.read(&path("Desktop/a.txt"), 0, 5).unwrap(), b"hello");
    }

    #[test]
    fn deleting_a_root_itself_is_refused() {
        let a = TempDir::new("delete-root-a");
        let b = TempDir::new("delete-root-b");
        let roots =
            Roots::open(vec![spec("Desktop", &a, true), spec("Downloads", &b, true)]).unwrap();
        assert_eq!(
            roots.delete(&path("Desktop")),
            Err(OpError::PermissionDenied)
        );
        assert_eq!(
            roots.mkdir(&path("Desktop")),
            Err(OpError::PermissionDenied)
        );
        // Both sides name real roots, so this proves the one-segment rule
        // fires before the cross-root `Unsupported` rule would otherwise
        // apply.
        assert_eq!(
            roots.rename(&path("Desktop"), &path("Downloads")),
            Err(OpError::PermissionDenied)
        );
    }

    #[test]
    fn rename_across_two_roots_is_unsupported() {
        let a = TempDir::new("rename-a");
        let b = TempDir::new("rename-b");
        std::fs::write(a.path.join("a.txt"), b"hi").unwrap();
        let roots =
            Roots::open(vec![spec("Desktop", &a, true), spec("Downloads", &b, true)]).unwrap();
        assert_eq!(
            roots.rename(&path("Desktop/a.txt"), &path("Downloads/a.txt")),
            Err(OpError::Unsupported)
        );
    }

    #[test]
    fn rename_within_one_root_works() {
        let dir = TempDir::new("rename-same");
        std::fs::write(dir.path.join("old.txt"), b"hi").unwrap();
        let roots = Roots::open(vec![spec("Desktop", &dir, true)]).unwrap();
        roots
            .rename(&path("Desktop/old.txt"), &path("Desktop/new.txt"))
            .unwrap();
        assert_eq!(roots.read(&path("Desktop/new.txt"), 0, 2).unwrap(), b"hi");
    }

    // `Roots` cannot derive `PartialEq` (it holds a `LocalFs`, which holds a
    // `Mutex`), so an open failure is checked with `unwrap_err` rather than
    // `assert_eq!` on the whole `Result`.

    #[test]
    fn an_empty_name_is_refused() {
        let dir = TempDir::new("name-empty");
        assert_eq!(
            Roots::open(vec![spec("", &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn a_name_over_64_bytes_is_refused() {
        let dir = TempDir::new("name-long");
        let long = "a".repeat(65);
        assert_eq!(
            Roots::open(vec![spec(&long, &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn a_name_with_a_slash_is_refused() {
        let dir = TempDir::new("name-slash");
        assert_eq!(
            Roots::open(vec![spec("a/b", &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn a_name_with_a_backslash_is_refused() {
        // `RemotePath::parse` in `path.rs` refuses a backslash, so a root
        // named with one could never be addressed as the first segment of
        // a path.
        let dir = TempDir::new("name-backslash");
        assert_eq!(
            Roots::open(vec![spec("a\\b", &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn a_name_with_a_control_character_is_refused() {
        let dir = TempDir::new("name-control");
        assert_eq!(
            Roots::open(vec![spec("a\u{0007}b", &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn a_name_of_a_single_dot_is_refused() {
        let dir = TempDir::new("name-dot");
        assert_eq!(
            Roots::open(vec![spec(".", &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn a_name_of_two_dots_is_refused() {
        let dir = TempDir::new("name-dotdot");
        assert_eq!(
            Roots::open(vec![spec("..", &dir, true)]).unwrap_err(),
            RootsError::RootNameInvalid
        );
    }

    #[test]
    fn names_differing_only_by_case_are_refused() {
        let a = TempDir::new("case-a");
        let b = TempDir::new("case-b");
        assert_eq!(
            Roots::open(vec![spec("Desktop", &a, true), spec("desktop", &b, true)]).unwrap_err(),
            RootsError::RootNameTaken
        );
    }

    #[test]
    fn two_roots_on_the_same_folder_are_refused() {
        let dir = TempDir::new("overlap-same");
        assert_eq!(
            Roots::open(vec![spec("First", &dir, true), spec("Second", &dir, true)]).unwrap_err(),
            RootsError::RootOverlaps
        );
    }

    #[test]
    fn a_root_nested_inside_another_is_refused_outer_first() {
        let parent = TempDir::new("overlap-nested-outer-first");
        let child = parent.path.join("Sub");
        std::fs::create_dir(&child).unwrap();
        let outer = RootSpec {
            name: "Outer".to_string(),
            path: parent.path.clone(),
            writable: true,
        };
        let inner = RootSpec {
            name: "Inner".to_string(),
            path: child,
            writable: true,
        };
        assert_eq!(
            Roots::open(vec![outer, inner]).unwrap_err(),
            RootsError::RootOverlaps
        );
    }

    #[test]
    fn a_root_nested_inside_another_is_refused_inner_first() {
        let parent = TempDir::new("overlap-nested-inner-first");
        let child = parent.path.join("Sub");
        std::fs::create_dir(&child).unwrap();
        let outer = RootSpec {
            name: "Outer".to_string(),
            path: parent.path.clone(),
            writable: true,
        };
        let inner = RootSpec {
            name: "Inner".to_string(),
            path: child,
            writable: true,
        };
        // Same two folders as above, given in the opposite order. The
        // overlap check must catch nesting whichever root is opened first.
        assert_eq!(
            Roots::open(vec![inner, outer]).unwrap_err(),
            RootsError::RootOverlaps
        );
    }

    #[test]
    fn sibling_folders_are_allowed() {
        let a = TempDir::new("sibling-a");
        let b = TempDir::new("sibling-b");
        std::fs::write(a.path.join("a.txt"), b"hi").unwrap();
        let roots = Roots::open(vec![spec("First", &a, true), spec("Second", &b, true)]).unwrap();
        assert_eq!(roots.read(&path("First/a.txt"), 0, 2).unwrap(), b"hi");
    }

    #[test]
    fn opening_with_no_roots_is_refused() {
        assert_eq!(Roots::open(Vec::new()).unwrap_err(), RootsError::NoRoots);
    }

    #[test]
    fn a_path_that_is_not_a_folder_is_refused() {
        let dir = TempDir::new("not-a-folder");
        let file = dir.path.join("a.txt");
        std::fs::write(&file, b"hi").unwrap();
        assert_eq!(
            Roots::open(vec![RootSpec {
                name: "Desktop".to_string(),
                path: file,
                writable: true,
            }])
            .unwrap_err(),
            RootsError::RootNotAFolder
        );
    }
}
