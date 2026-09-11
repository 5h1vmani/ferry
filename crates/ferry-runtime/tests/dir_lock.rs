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
//! `make_engine` and `code_of_error` come from `tests/common/paths.rs`,
//! the by-hand-peer harness every other file here already shares.

mod common;

use std::sync::Arc;

use common::paths::{Inbox, code_of_error, make_engine};
use ferry_runtime::generate_key;

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
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "dir_lock test",
        key,
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    )
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
    let inbox = Arc::new(Inbox::default());

    let first_key = generate_key().expect("a fresh key pair");
    let first = make_engine(
        "dir_lock test",
        first_key,
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    )
    .expect("the first engine should build");

    let second_key = generate_key().expect("a fresh key pair");
    let second = make_engine(
        "dir_lock test",
        second_key,
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    );
    let error = second.err().expect("a second live engine must be refused");
    assert_eq!(code_of_error(&error), "Runtime::BadConfig");

    first.stop();

    let third_key = generate_key().expect("a fresh key pair");
    let third = make_engine(
        "dir_lock test",
        third_key,
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    )
    .expect("the folder is free once the first engine stopped");
    third.stop();
}
