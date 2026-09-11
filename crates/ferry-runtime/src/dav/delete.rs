//! Planning a recursive `DELETE`.
//!
//! `docs/engine-contract.md`, item 6, I2: "a folder is walked with
//! `folder.rs`'s bounds and deleted leaves first, then folders deepest
//! first; the wire stays non-recursive." [`plan`] does the walk and
//! returns what to delete and in what order; `server.rs` does the actual
//! deleting, one `delete` call at a time, so a peer failure partway
//! through is visible to the caller as an ordinary RPC error.
//!
//! This reuses [`crate::folder::MAX_FOLDER_FILES`],
//! [`crate::folder::MAX_FOLDER_DEPTH`], and [`crate::folder::after_page`]
//! rather than inventing new bounds: the same walk `pull_folder` already
//! proves safe against a folder that never finishes paging, or nests
//! without end.

use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::FileOps;

use crate::folder::{MAX_FOLDER_DEPTH, MAX_FOLDER_FILES, after_page};

/// Why [`plan`] could not finish.
#[derive(Debug)]
pub(crate) enum PlanError {
    /// More files or folders than `folder.rs`'s bounds allow. The caller
    /// answers 507, per item 6.
    TooLarge,
    /// A call to [`FileOps::list`] itself failed.
    Op(OpError),
}

/// Every file and folder a recursive `DELETE` of `root` must remove, in
/// the order to remove them: every file, in the order the walk found
/// them, then every folder, deepest first, ending with `root` itself.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) files: Vec<RemotePath>,
    pub(crate) dirs: Vec<RemotePath>,
}

impl Plan {
    /// How many files this plan removes, for the folder's own access log
    /// entry (`EntryFields::files`), the same field a folder copy uses.
    pub(crate) fn file_count(&self) -> u32 {
        u32::try_from(self.files.len()).unwrap_or(u32::MAX)
    }
}

/// Walks `root` and everything under it, within `folder.rs`'s bounds.
///
/// # Errors
///
/// Returns [`PlanError::TooLarge`] past [`MAX_FOLDER_FILES`] files or
/// [`MAX_FOLDER_DEPTH`] levels of nesting, or when one folder's own
/// listing runs past what [`after_page`] accepts. Returns
/// [`PlanError::Op`] when a call to `fs.list` fails.
pub(crate) fn plan(fs: &dyn FileOps, root: &RemotePath) -> Result<Plan, PlanError> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut total = 0usize;
    walk(fs, root, 1, &mut files, &mut dirs, &mut total)?;
    Ok(Plan { files, dirs })
}

/// Lists `dir` and recurses into every subfolder before pushing `dir`
/// itself onto `dirs`: a directory's own entry only lands once every
/// entry inside it, direct or nested, already has, which is what makes
/// `dirs` come out deepest first, `root` last, with no separate sort.
///
/// `total` counts every file and every folder found anywhere in the walk
/// so far, against the one [`MAX_FOLDER_FILES`] budget, the same shared
/// counter `folder::list_recursive` uses: a folder's own listing is
/// bounded on its own by [`after_page`], but that resets for each folder
/// walked, so nothing before this counter stopped a peer whose every
/// folder answers within bounds from still fanning out to a huge total
/// across many folders.
fn walk(
    fs: &dyn FileOps,
    dir: &RemotePath,
    depth: u32,
    files: &mut Vec<RemotePath>,
    dirs: &mut Vec<RemotePath>,
    total: &mut usize,
) -> Result<(), PlanError> {
    if depth > MAX_FOLDER_DEPTH {
        return Err(PlanError::TooLarge);
    }
    let mut cursor = 0u64;
    let mut pages = 0usize;
    let mut entries_seen = 0usize;
    loop {
        let (entries, next) = fs.list(dir, cursor).map_err(PlanError::Op)?;
        pages += 1;
        entries_seen += entries.len();
        for entry in entries {
            let child = join(dir, &entry.name)?;
            if child == *dir {
                continue;
            }
            *total += 1;
            if *total > MAX_FOLDER_FILES {
                return Err(PlanError::TooLarge);
            }
            match entry.kind {
                FileKind::File => files.push(child),
                FileKind::Directory => walk(fs, &child, depth + 1, files, dirs, total)?,
            }
        }
        match after_page(cursor, next, pages, entries_seen) {
            Ok(Some(next_cursor)) => cursor = next_cursor,
            Ok(None) => break,
            Err(_) => return Err(PlanError::TooLarge),
        }
    }
    dirs.push(dir.clone());
    Ok(())
}

fn join(dir: &RemotePath, name: &str) -> Result<RemotePath, PlanError> {
    let text = if dir.is_root() {
        name.to_owned()
    } else {
        format!("{}/{name}", dir.as_str())
    };
    RemotePath::parse(&text).map_err(|_| PlanError::Op(OpError::InvalidPath))
}

#[cfg(test)]
mod tests {
    use ferry_core::memfs::MemoryFs;
    use ferry_core::ops::{Entry, FileKind, OpError};
    use ferry_core::path::RemotePath;
    use ferry_core::rpc::FileOps;

    use super::{PlanError, plan};

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    /// A fake peer that answers every `list` with a folder full of empty
    /// subfolders, exactly two levels deep, the same shape
    /// `folder::ManyFoldersFs` uses to prove `list_recursive`'s identical
    /// bound: no single folder's own listing goes over
    /// [`super::MAX_FOLDER_FILES`], so only a walk that counts folders and
    /// files in one shared budget, across the whole tree, catches this
    /// before `DELETE` plans to remove upward of ten thousand of them.
    struct ManyFoldersFs {
        branch: usize,
    }

    impl FileOps for ManyFoldersFs {
        fn list(
            &self,
            path: &RemotePath,
            _cursor: u64,
        ) -> Result<(Vec<Entry>, Option<u64>), OpError> {
            let depth = path.as_str().matches('/').count();
            if depth >= 2 {
                return Ok((Vec::new(), None));
            }
            let entries = (0..self.branch)
                .map(|i| Entry {
                    name: format!("sub{i}"),
                    kind: FileKind::Directory,
                    size: 0,
                    modified_unix_secs: 0,
                })
                .collect();
            Ok((entries, None))
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
        fn manifest(&self, _path: &RemotePath) -> Result<ferry_core::chunk::Manifest, OpError> {
            Err(OpError::Unsupported)
        }
    }

    #[test]
    fn many_folders_across_the_tree_are_bounded_the_same_as_many_files() {
        // 101 root children times 101 grandchildren each is 10,302 folders
        // total, all within bounds one folder at a time, and well past
        // `MAX_FOLDER_FILES` in total. Answering 507 here, with nothing
        // deleted, is what `server::delete_verb` relies on: `plan` never
        // returns a plan this large for the caller to act on.
        let fs = ManyFoldersFs { branch: 101 };
        let error = plan(&fs, &path("Camera")).expect_err("folders share the file budget too");
        assert!(matches!(error, PlanError::TooLarge));
    }

    #[test]
    fn a_nested_folder_deletes_leaves_first_then_folders_deepest_first() {
        let fs = MemoryFs::new();
        fs.insert_file("Camera/a.jpg", Vec::new());
        fs.insert_dir("Camera/Sub");
        fs.insert_file("Camera/Sub/b.jpg", Vec::new());

        let plan = plan(&fs, &path("Camera")).expect("within bounds");
        // Files are independent leaves, so the order between them is not
        // meaningful; only that both are present matters here.
        let mut files: Vec<&str> = plan.files.iter().map(RemotePath::as_str).collect();
        files.sort_unstable();
        assert_eq!(files, vec!["Camera/Sub/b.jpg", "Camera/a.jpg"]);

        let dirs: Vec<&str> = plan.dirs.iter().map(RemotePath::as_str).collect();
        assert_eq!(
            dirs,
            vec!["Camera/Sub", "Camera"],
            "deepest first, root last"
        );
        assert_eq!(plan.file_count(), 2);
    }

    #[test]
    fn an_empty_folder_plans_only_itself() {
        let fs = MemoryFs::new();
        fs.insert_dir("Camera");
        let plan = plan(&fs, &path("Camera")).expect("an empty folder is within bounds");
        assert!(plan.files.is_empty());
        assert_eq!(
            plan.dirs.iter().map(RemotePath::as_str).collect::<Vec<_>>(),
            vec!["Camera"]
        );
    }

    #[test]
    fn one_file_over_the_cap_is_too_large() {
        let fs = MemoryFs::new();
        for i in 0..=super::MAX_FOLDER_FILES {
            fs.insert_file(&format!("Camera/{i:05}.jpg"), Vec::new());
        }
        let error = plan(&fs, &path("Camera")).expect_err("one over the cap");
        assert!(matches!(error, PlanError::TooLarge));
    }
}
