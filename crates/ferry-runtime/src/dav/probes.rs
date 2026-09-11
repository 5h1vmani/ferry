//! Apple's metadata probes, matched and answered locally, and the sidecar
//! store that lets Finder write `.DS_Store` without the peer ever seeing
//! it.
//!
//! `docs/engine-contract.md`, item 6: every probe name is answered locally
//! and never reaches the peer, and a listing never shows a sidecar. The
//! second half holds on its own: a listing is always the peer's own
//! `list()` result, and a sidecar is never added to it, so there is
//! nothing to filter out.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::state::now_unix_secs;

/// True when `name`, a path's last segment, is one of Apple's metadata
/// probes.
#[must_use]
pub(crate) fn is_probe_name(name: &str) -> bool {
    name.starts_with("._")
        || name.starts_with(".ql_")
        || matches!(
            name,
            ".DS_Store"
                | ".hidden"
                | ".Spotlight-V100"
                | ".metadata_never_index"
                | ".Trashes"
                | ".fseventsd"
                | ".TemporaryItems"
        )
}

/// The last `/`-separated segment of a decoded DAV path. Empty for the
/// root.
#[must_use]
pub(crate) fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One sidecar file's stored bytes and modified time.
pub(crate) struct Sidecar {
    pub(crate) bytes: Vec<u8>,
    pub(crate) modified_unix_secs: i64,
}

/// Where one device's sidecar files live: `data_dir/dav_sidecars/<device
/// key hex>/`, mirroring the DAV path each one sidecars. Never reached by
/// the peer, and never returned by a listing.
pub(crate) struct SidecarStore {
    base: PathBuf,
}

impl SidecarStore {
    pub(crate) fn new(base: PathBuf) -> Self {
        Self { base }
    }

    /// Where `path` would live on disk, or `None` for a path this store
    /// will not write outside its own base folder. A path built from a
    /// percent-decoded DAV target could still hold a NUL byte or a `..`
    /// segment; nothing upstream of this store has already refused those,
    /// so it checks for itself.
    fn disk_path(&self, path: &str) -> Option<PathBuf> {
        if path.is_empty() || path.contains('\0') {
            return None;
        }
        for part in path.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                return None;
            }
        }
        Some(self.base.join(path))
    }

    /// The stored sidecar for `path`, or `None` when nothing was ever
    /// written there. `docs/engine-contract.md`, item 6: "a read of one is
    /// 404" is this returning `None`.
    pub(crate) fn read(&self, path: &str) -> Option<Sidecar> {
        let disk_path = self.disk_path(path)?;
        let bytes = std::fs::read(&disk_path).ok()?;
        let modified_unix_secs = modified_time(&disk_path).unwrap_or_else(now_unix_secs);
        Some(Sidecar {
            bytes,
            modified_unix_secs,
        })
    }

    /// Stores `bytes` as the sidecar for `path`, making its parent folder
    /// if needed.
    pub(crate) fn write(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        let disk_path = self
            .disk_path(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsafe sidecar path"))?;
        if let Some(parent) = disk_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(disk_path, bytes)
    }
}

fn modified_time(path: &Path) -> Option<i64> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let secs = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::{SidecarStore, is_probe_name, last_segment};

    #[test]
    fn matches_every_name_item_6_lists() {
        for name in [
            ".DS_Store",
            ".hidden",
            ".Spotlight-V100",
            ".metadata_never_index",
            ".Trashes",
            ".fseventsd",
            ".TemporaryItems",
            "._IMG_0001.jpg",
            ".ql_disablethumbnails",
            ".ql_disablecache",
        ] {
            assert!(is_probe_name(name), "{name} should match");
        }
    }

    #[test]
    fn an_ordinary_name_does_not_match() {
        assert!(!is_probe_name("IMG_0001.jpg"));
        assert!(!is_probe_name("Q3 notes.md"));
    }

    #[test]
    fn last_segment_reads_the_final_component() {
        assert_eq!(last_segment("DCIM/Camera/.DS_Store"), ".DS_Store");
        assert_eq!(last_segment(".DS_Store"), ".DS_Store");
        assert_eq!(last_segment(""), "");
    }

    #[test]
    fn a_sidecar_reads_back_what_was_written() {
        let dir = std::env::temp_dir().join(format!("ferry-dav-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SidecarStore::new(dir.clone());
        store.write("DCIM/.DS_Store", b"hello").unwrap();
        let sidecar = store.read("DCIM/.DS_Store").expect("just written");
        assert_eq!(sidecar.bytes, b"hello");
        assert!(store.read("DCIM/.other").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_traversal_attempt_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("ferry-dav-sidecar-escape-{}", std::process::id()));
        let store = SidecarStore::new(dir);
        assert!(store.write("../escape", b"x").is_err());
        assert!(store.read("../escape").is_none());
    }
}
