//! Regression tests for audit `docs/audits/kotlin-lifecycle.md` finding 1.
//!
//! `data_dir/lock` used to be a file made with `create_new`: its existence
//! alone was the lock, so a process the system killed left it behind and
//! every later `Engine::new` on that directory failed forever. The fix
//! makes it an OS advisory lock the kernel drops the instant the holding
//! process ends, killed or not. These two tests are the ones that must
//! flip: a stale file must stop blocking a new engine, and a live engine
//! must still block a second one on the same folder.
//!
//! The helpers below are the smallest slice of `tests/common/paths.rs` needed to
//! build an engine: a no-op listener and a `make_engine` that mirrors the
//! one there.

use std::path::Path;

use ferry_runtime::{
    Config, DeviceKind as RuntimeDeviceKind, Engine, EngineListener, FerryError, KeyPair,
    PairingState, Root, generate_key,
};

/// A listener that does nothing. These tests never need a callback.
struct Silent;

impl EngineListener for Silent {
    fn devices_changed(&self) {}
    fn transfers_changed(&self) {}
    fn pairing_changed(&self, _state: PairingState) {}
    fn access_log_changed(&self) {}
}

/// Build an engine on the given folders. It is not started, so this
/// exercises only `Engine::new`, which is where `DirLock::take` runs.
fn make_engine(
    key: KeyPair,
    data: &Path,
    shared: &Path,
    download: &Path,
) -> Result<std::sync::Arc<Engine>, FerryError> {
    Engine::new(
        Config {
            data_dir: data.to_string_lossy().into_owned(),
            shared_roots: vec![Root {
                name: "Root".to_owned(),
                path: shared.to_string_lossy().into_owned(),
                writable: true,
            }],
            download_dir: download.to_string_lossy().into_owned(),
            display_name: "dir_lock test".to_owned(),
            listen_port: 0,
            key,
            kind: RuntimeDeviceKind::Mac,
        },
        Box::new(Silent),
    )
}

/// The code an error carries, or a panic saying it had none.
fn code_of_error(error: &FerryError) -> String {
    let FerryError::Failed { code, .. } = error;
    code.clone()
}

/// A stale `lock` file, written by hand with no process holding it, must
/// not stop a new engine. Before the fix, `create_new` refused to make a
/// file that already exists, so this failed with `Runtime::BadConfig` even
/// though nothing was alive to hold the folder.
#[test]
fn a_stale_lock_file_does_not_block_a_new_engine() {
    let data = tempfile::tempdir().expect("a temporary data folder");
    let shared = tempfile::tempdir().expect("a temporary shared folder");
    let download = tempfile::tempdir().expect("a temporary download folder");

    // A crash leaves a file with the dead process's id in it, and nothing
    // holding any lock on it.
    std::fs::write(data.path().join("lock"), b"999999\n").expect("write the stale lock file");

    let key = generate_key().expect("a fresh key pair");
    let engine = make_engine(key, data.path(), shared.path(), download.path())
        .expect("a stale lock file left by a dead process must not block a new engine");
    engine.stop();
}

/// A second `Engine::new` on a folder whose first engine is still alive is
/// refused with the same code as before the fix. Once that engine calls
/// `stop`, which releases the OS lock, a new engine on the same folder
/// succeeds.
#[test]
fn a_live_engine_still_blocks_a_second_one_until_it_stops() {
    let data = tempfile::tempdir().expect("a temporary data folder");
    let shared = tempfile::tempdir().expect("a temporary shared folder");
    let download = tempfile::tempdir().expect("a temporary download folder");

    let first_key = generate_key().expect("a fresh key pair");
    let first = make_engine(first_key, data.path(), shared.path(), download.path())
        .expect("the first engine should build");

    let second_key = generate_key().expect("a fresh key pair");
    let second = make_engine(second_key, data.path(), shared.path(), download.path());
    let error = second.err().expect("a second live engine must be refused");
    assert_eq!(code_of_error(&error), "Runtime::BadConfig");

    first.stop();

    let third_key = generate_key().expect("a fresh key pair");
    let third = make_engine(third_key, data.path(), shared.path(), download.path())
        .expect("the folder is free once the first engine stopped");
    third.stop();
}
