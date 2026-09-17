//! `landing_folder`, `docs/engine-contract.md` item 5's "Where a push
//! lands", now decided once by the engine instead of twice, in Swift and in
//! Kotlin.
//!
//! Two engines in one process, paired the way `common::engines::pair`
//! pairs them. The "mac" role in `pair` is only ever the calling side here;
//! the peer's real `DeviceKind` and roots are what each test sets up, since
//! `landing_folder` decides from those, not from which role dialled first.

mod common;

use std::sync::Arc;

use common::engines::{build_as, code_of_error, pair};
use common::paths::{build, key_hex, pair_with_peer, start_peer_with};

use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::FileOps;
use ferry_runtime::{DeviceKind, Root};

/// A Mac peer with two roots lands in the one named `Downloads`, ignoring
/// that `Desktop` was listed first.
#[test]
fn a_mac_peer_with_desktop_and_downloads_lands_in_downloads_ferry() {
    let caller = build_as("Vamana", DeviceKind::Mac);
    let peer = build_as("Amaravati", DeviceKind::Mac);

    let desktop = peer.shared.path().join("Desktop");
    let downloads = peer.shared.path().join("Downloads");
    std::fs::create_dir(&desktop).expect("the Desktop folder should be makeable");
    std::fs::create_dir(&downloads).expect("the Downloads folder should be makeable");
    peer.engine
        .set_roots(vec![
            Root {
                name: "Desktop".to_owned(),
                path: desktop.to_string_lossy().into_owned(),
                writable: true,
            },
            Root {
                name: "Downloads".to_owned(),
                path: downloads.to_string_lossy().into_owned(),
                writable: true,
            },
        ])
        .expect("two writable roots should be accepted");

    let peer_key = pair(&caller, &peer);

    let folder = caller
        .engine
        .landing_folder(peer_key)
        .expect("a Mac peer with a Downloads root should land there");
    assert_eq!(folder, "Downloads/Ferry");
    assert!(
        downloads.join("Ferry").is_dir(),
        "landing_folder must actually create the folder on the peer"
    );

    caller.engine.stop();
    peer.engine.stop();
}

/// A Mac peer with no root named `Downloads` lands in its first root
/// instead.
#[test]
fn a_mac_peer_with_one_root_named_stuff_lands_in_stuff_ferry() {
    let caller = build_as("Vamana", DeviceKind::Mac);
    let peer = build_as("Amaravati", DeviceKind::Mac);

    peer.engine
        .set_roots(vec![Root {
            name: "Stuff".to_owned(),
            path: peer.shared_root.to_string_lossy().into_owned(),
            writable: true,
        }])
        .expect("one writable root should be accepted");

    let peer_key = pair(&caller, &peer);

    let folder = caller
        .engine
        .landing_folder(peer_key)
        .expect("a Mac peer with no Downloads root should still land somewhere");
    assert_eq!(folder, "Stuff/Ferry");
    assert!(peer.shared_root.join("Ferry").is_dir());

    caller.engine.stop();
    peer.engine.stop();
}

/// A phone peer lands in its first root, in a folder named `Download`, not
/// `Ferry`: the two kinds keep their own fixed name.
#[test]
fn a_phone_peer_lands_in_its_first_root_slash_download() {
    let caller = build_as("Vamana", DeviceKind::Mac);
    let peer = build_as("Pixel 3 XL", DeviceKind::Phone);

    let peer_key = pair(&caller, &peer);

    let folder = caller
        .engine
        .landing_folder(peer_key)
        .expect("a phone peer should land in its first root");
    assert_eq!(folder, "Root/Download");
    assert!(peer.shared_root.join("Download").is_dir());

    caller.engine.stop();
    peer.engine.stop();
}

/// A second call for the same peer still succeeds, because the second
/// `mkdir` answers `OpError::AlreadyExists`, which `landing_folder` treats
/// as success rather than a failure.
#[test]
fn a_second_call_for_the_same_peer_still_succeeds() {
    let caller = build_as("Vamana", DeviceKind::Mac);
    let peer = build_as("Amaravati", DeviceKind::Mac);
    let peer_key = pair(&caller, &peer);

    let first = caller
        .engine
        .landing_folder(peer_key.clone())
        .expect("the first call should create the folder");
    let second = caller
        .engine
        .landing_folder(peer_key)
        .expect("the second call must succeed: AlreadyExists is success");
    assert_eq!(first, second);

    caller.engine.stop();
    peer.engine.stop();
}

/// A read-only root cannot be made into a landing folder, so `mkdir`
/// answers `OpError::PermissionDenied`, and `landing_folder` reports it
/// rather than treating it as success.
#[test]
fn a_read_only_root_returns_permission_denied() {
    let caller = build_as("Vamana", DeviceKind::Mac);
    let peer = build_as("Amaravati", DeviceKind::Mac);

    peer.engine
        .set_roots(vec![Root {
            name: "Root".to_owned(),
            path: peer.shared_root.to_string_lossy().into_owned(),
            writable: false,
        }])
        .expect("one read-only root should be accepted");

    let peer_key = pair(&caller, &peer);

    let error = caller
        .engine
        .landing_folder(peer_key)
        .expect_err("a read-only root must refuse the landing folder");
    assert_eq!(code_of_error(&error), "OpError::PermissionDenied");
    assert!(
        !peer.shared_root.join("Ferry").is_dir(),
        "a refused mkdir must not create the folder"
    );

    caller.engine.stop();
    peer.engine.stop();
}

/// Row 13, `docs/audits/principles-fixes.md`: a peer whose one root is
/// named `.`.
///
/// A real `Engine` peer cannot be made to list a name this bad: its own
/// `set_roots` refuses one before it is ever stored. So this speaks the
/// wire directly, the way `transfer_paths.rs`'s `EmptyNameFs` does for a
/// peer that names an entry badly.
struct BadRootNameFs;

impl FileOps for BadRootNameFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: ".".to_owned(),
                kind: FileKind::Directory,
                size: 0,
                modified_unix_secs: 1_000_000,
            }],
            None,
        ))
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

/// A peer that lists a root named `.` is refused, not joined into the
/// folder path `landing_folder` returns.
#[test]
fn a_peer_that_lists_a_bad_root_name_is_refused() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(BadRootNameFs));
    pair_with_peer(&side, &peer);

    let error = side
        .engine
        .landing_folder(key_hex(&peer.key))
        .expect_err("a root named . must be refused, not joined into a path");
    assert_eq!(code_of_error(&error), "OpError::InvalidPath");

    peer.close();
}

/// Row 3, `docs/audits/principles-fixes.md`: a peer whose root list is
/// empty.
struct NoRootsFs;

impl FileOps for NoRootsFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((vec![], None))
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

/// A peer that shares no folder at all is named in the error, not blamed
/// on this device's own empty-list code.
#[test]
fn a_peer_that_shares_nothing_is_named_in_the_error() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(NoRootsFs));
    pair_with_peer(&side, &peer);

    let error = side
        .engine
        .landing_folder(key_hex(&peer.key))
        .expect_err("a peer with no roots has nowhere for a push to land");
    assert_eq!(code_of_error(&error), "Runtime::PeerSharesNothing");

    peer.close();
}
