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
    walk(fs, root, 1, &mut files, &mut dirs)?;
    Ok(Plan { files, dirs })
}

/// Lists `dir` and recurses into every subfolder before pushing `dir`
/// itself onto `dirs`: a directory's own entry only lands once every
/// entry inside it, direct or nested, already has, which is what makes
/// `dirs` come out deepest first, `root` last, with no separate sort.
fn walk(
    fs: &dyn FileOps,
    dir: &RemotePath,
    depth: u32,
    files: &mut Vec<RemotePath>,
    dirs: &mut Vec<RemotePath>,
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
            match entry.kind {
                FileKind::File => {
                    files.push(child);
                    if files.len() > MAX_FOLDER_FILES {
                        return Err(PlanError::TooLarge);
                    }
                }
                FileKind::Directory => walk(fs, &child, depth + 1, files, dirs)?,
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
    use ferry_core::path::RemotePath;

    use super::{PlanError, plan};

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
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
