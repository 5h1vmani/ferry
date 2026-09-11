//! `docs/engine-contract.md` item 5, the fix pass's guard extended to
//! `push_files`.
//!
//! `push` already refuses a second live push to the same destination on the
//! same device with `Runtime::PushInFlight` (`crates/ferry-runtime/src/push.rs`,
//! `live_push_exists`), because two live pushes to one path would race to
//! write the same `.ferry-part` file. `push_files` had no such guard: two
//! batches to the same folder, or one batch whose own local paths map to the
//! same remote name, could still collide. This file is that guard's own
//! test file, one file per item, per `docs/agent-runs.md` rule 3.
//!
//! The peer-and-engine harness comes from `tests/common/paths.rs`, which
//! every by-hand-peer test file here shares; `batch_ids` and `wait_batch`
//! below are specific to this file's own batch-level checks.

mod common;

use std::sync::Arc;

use common::paths::{Side, build, code_of_error, mib, pair_two_engines, poll_until, sample_bytes};
use ferry_runtime::{BatchInfo, TransferState};

/// Every batch's id, for a plain length or membership check.
fn batch_ids(mac: &Side) -> Vec<String> {
    mac.engine
        .batches()
        .into_iter()
        .map(|b: BatchInfo| b.id)
        .collect()
}

/// Wait until one batch satisfies `check`.
fn wait_batch(mac: &Side, id: &str, what: &str, check: impl Fn(&BatchInfo) -> bool) {
    let engine = Arc::clone(&mac.engine);
    let wanted = id.to_owned();
    poll_until(what, move || {
        engine.batches().iter().any(|b| b.id == wanted && check(b))
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A second `push_files` call into the same folder, while the first batch is
/// still live, collides on the same destination name and must be refused
/// whole -- no batch, no transfer rows -- exactly as a second single `push`
/// to that name already is. Once the first batch finishes, the same call
/// succeeds: the guard is about two live pushes at once, never about a name
/// a finished push once used.
#[test]
fn a_second_push_files_call_that_collides_with_a_live_batch_is_refused() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Uploads"))
        .expect("a folder on the phone to push into");

    let source_one = tempfile::tempdir().expect("a folder for the first push's files");
    let path_a = source_one.path().join("shared.bin");
    std::fs::write(&path_a, sample_bytes(mib(1))).expect("shared.bin should write");

    let first_batch = mac
        .engine
        .push_files(
            phone_key.clone(),
            vec![path_a.to_string_lossy().into_owned()],
            "Root/Uploads".to_owned(),
        )
        .expect("the first push_files call should be accepted");

    // A second source folder, so the colliding local path is not literally
    // the same string as the first -- only its leaf name, and so its
    // destination inside "Root/Uploads", matches.
    let source_two = tempfile::tempdir().expect("a folder for the second push's files");
    let path_b = source_two.path().join("shared.bin");
    std::fs::write(&path_b, sample_bytes(mib(1))).expect("the colliding file should write");

    let second = mac.engine.push_files(
        phone_key.clone(),
        vec![path_b.to_string_lossy().into_owned()],
        "Root/Uploads".to_owned(),
    );
    assert_eq!(
        second.err().map(|error| code_of_error(&error)),
        Some("Runtime::PushInFlight".to_owned()),
        "a push_files call that collides with a live push must be refused whole"
    );

    assert_eq!(
        batch_ids(&mac),
        vec![first_batch.clone()],
        "the refused call must create no batch of its own"
    );
    assert_eq!(
        mac.engine.transfers().len(),
        1,
        "the refused call must create no transfer row of its own"
    );

    wait_batch(&mac, &first_batch, "the first batch to finish", |b| {
        b.state == TransferState::Done
    });

    // Once the first batch has finished, "Root/Uploads/shared.bin" is free
    // again: this is about two live pushes racing, never about a name a
    // finished push once used.
    let third_batch = mac
        .engine
        .push_files(
            phone_key,
            vec![path_b.to_string_lossy().into_owned()],
            "Root/Uploads".to_owned(),
        )
        .expect("the same call should succeed once the first batch has finished");
    wait_batch(&mac, &third_batch, "the third batch to finish", |b| {
        b.state == TransferState::Done
    });

    mac.engine.stop();
    phone.engine.stop();
}

/// Two local paths whose leaf names collide inside the same
/// `remote_folder` would race each other into the same `.ferry-part` file
/// just as surely as two separate live pushes would. The whole call is
/// refused, and nothing is queued: not even the file whose name did not
/// collide with anything.
#[test]
fn push_files_with_local_paths_sharing_a_leaf_name_is_refused() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Uploads"))
        .expect("a folder on the phone to push into");

    let source_one = tempfile::tempdir().expect("a folder for the first file");
    let path_a = source_one.path().join("dup.bin");
    std::fs::write(&path_a, sample_bytes(mib(1))).expect("dup.bin should write");

    let source_two = tempfile::tempdir().expect("a folder for the second file");
    let path_b = source_two.path().join("dup.bin");
    std::fs::write(&path_b, sample_bytes(mib(1))).expect("the second dup.bin should write");

    let result = mac.engine.push_files(
        phone_key,
        vec![
            path_a.to_string_lossy().into_owned(),
            path_b.to_string_lossy().into_owned(),
        ],
        "Root/Uploads".to_owned(),
    );
    assert_eq!(
        result.err().map(|error| code_of_error(&error)),
        Some("Runtime::PushInFlight".to_owned()),
        "two local paths that map to the same remote name must refuse the whole call"
    );

    assert!(
        mac.engine.batches().is_empty(),
        "a call refused before anything is queued must create no batch"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "a call refused before anything is queued must create no transfer row"
    );

    mac.engine.stop();
    phone.engine.stop();
}
