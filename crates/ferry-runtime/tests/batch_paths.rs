//! Batches across a restart, and the bounds on a stored batch.
//!
//! `docs/engine-contract.md`, batch D, item 2, and the batch D audit,
//! findings D4, D5, D6 and D8: a batch's grouping and done count survive a
//! restart, a garbage batch file is removed at start, a finished batch is
//! dropped, and a failed batch reports its error.
//!
//! Some tests build a peer by hand. That harness is
//! `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, MIB, build, code_of_error, key_hex, make_engine, mib, pair_two_engines, pair_with_peer,
    poll_until, sample_bytes, start_peer_with, wait_transfer,
};

use std::sync::Arc;

use ferry_core::chunk::Manifest;
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::FileOps;
use ferry_runtime::TransferState;

/// A filesystem with one file that always fails once it is fetched: `list`
/// finds it, but `stat` refuses it. Stands in for a peer whose one file
/// cannot be read, so the transfer it becomes reaches `Failed` at once.
struct AlwaysMissingFs;

impl FileOps for AlwaysMissingFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: "missing.bin".to_owned(),
                kind: FileKind::File,
                size: 4,
                modified_unix_secs: 1_000_000,
            }],
            None,
        ))
    }
    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::NotFound)
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
    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Err(OpError::Unsupported)
    }
}

#[test]
fn a_restart_keeps_a_batchs_grouping() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    std::fs::write(
        phone.shared_root().join("Camera/big.bin"),
        sample_bytes(mib(8)),
    )
    .expect("the phone's shared folder should accept a file");

    // `pull_folder`'s own listing dial never wraps its stream in `Cut`, only
    // a transfer attempt's dial does (`transfer::attempt`), so the cut armed
    // here waits untouched through the listing and lands on the one file's
    // own dial, the same technique `an_interrupted_first_pass_resumes_after_a_restart`
    // uses through a hand-built peer.
    mac.engine.set_cut(2 * MIB);

    let batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("the transfer to pause after the cut", move || {
        engine.transfers().iter().any(|t| {
            t.batch_id.as_deref() == Some(wanted.as_str()) && t.state == TransferState::Paused
        })
    });

    let before_ids: Vec<String> = mac
        .engine
        .transfers()
        .into_iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch_id.as_str()))
        .map(|t| t.id)
        .collect();
    assert_eq!(
        before_ids.len(),
        1,
        "the one file queued is the one transfer"
    );

    mac.engine.stop();
    phone.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let batch = engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch should still be listed after a restart");
    assert_eq!(
        batch.state,
        TransferState::Paused,
        "the batch's one surviving transfer is still paused"
    );
    let after_ids: Vec<String> = engine
        .transfers()
        .into_iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch.id.as_str()))
        .map(|t| t.id)
        .collect();
    assert_eq!(
        after_ids, before_ids,
        "the same transfer ids are still grouped under the batch"
    );

    engine.stop();
}

/// Batch D audit, D4: a batch's done count survives a restart.
#[test]
fn a_batch_with_some_files_done_before_a_restart_still_reports_them_done_after() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    // Two files the same size, so it does not matter which one `set_cut`
    // below happens to land on: `pull_folder` queues both at once and a
    // pool of workers may dial either first, but whichever dial is not the
    // one `set_cut` is armed for runs uncut and reaches Done in full, while
    // the other pauses partway.
    std::fs::write(
        phone.shared_root().join("Camera/a.bin"),
        sample_bytes(mib(4)),
    )
    .expect("the phone's shared folder should accept the first file");
    std::fs::write(
        phone.shared_root().join("Camera/b.bin"),
        sample_bytes(mib(4)),
    )
    .expect("the phone's shared folder should accept the second file");

    // `set_cut` is one shot (`Option::take` in `transfer::attempt`), so only
    // the first dial to reach it is cut; the other transfer's dial finds
    // nothing armed and runs to completion. 2 MiB is short of each file's
    // 4 MiB, so the cut one pauses partway rather than finishing anyway.
    mac.engine.set_cut(2 * MIB);

    let batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("one file to finish and the other to pause", move || {
        let transfers = engine.transfers();
        let done = transfers
            .iter()
            .filter(|t| t.batch_id.as_deref() == Some(wanted.as_str()))
            .filter(|t| t.state == TransferState::Done)
            .count();
        let paused = transfers
            .iter()
            .filter(|t| t.batch_id.as_deref() == Some(wanted.as_str()))
            .filter(|t| t.state == TransferState::Paused)
            .count();
        done == 1 && paused == 1
    });
    let before_stop = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch is listed");
    assert_eq!(before_stop.files_done, 1, "one of the two files is done");

    mac.engine.stop();
    phone.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    // The done file's own record does not survive: `run_once` removed it
    // the moment it finished. Only the paused file's record does.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let surviving: Vec<_> = engine
        .transfers()
        .into_iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch_id.as_str()))
        .collect();
    assert_eq!(
        surviving.len(),
        1,
        "only the paused transfer's own row survives the restart"
    );

    // This is the audit's "0 of 100, then 40 of 100, forever": without the
    // stored floor, files_done would read 0 here, because the done file's
    // row is gone and the live count alone has nothing left to count it
    // from.
    let after = engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch is still listed after a restart");
    assert_eq!(
        after.files_done, 1,
        "the batch's stored floor still says one file is done, not zero"
    );

    engine.stop();
}

/// Batch D audit, D5 and D6: a bound on a stored batch's count, and cleanup
/// of one that does not decode.
#[test]
fn a_garbage_batch_file_is_removed_when_the_engine_starts() {
    let side = build("Vamana");
    side.engine.stop();

    let batches_dir = side.data.path().join("batches");
    // A name shaped like a real batch id, `<device key>-<session>`, so it is
    // not skipped for that reason first; its contents are what do not
    // decode.
    let garbage_path = batches_dir
        .join("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef-garbage");
    std::fs::write(&garbage_path, b"not a batch record").expect("the garbage file should write");
    assert!(
        garbage_path.exists(),
        "the garbage file exists before the restart"
    );

    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    )
    .expect("the engine should build even with a garbage batch file present");

    assert!(
        !garbage_path.exists(),
        "a batch file that does not decode is removed, not left for every future start to skip"
    );
    assert!(
        engine.batches().is_empty(),
        "the garbage file names no real batch"
    );
}

/// Batch D audit, D8: behaviours the audit found missing tests for.
#[test]
fn pull_folder_creates_nothing_when_the_peer_folder_is_too_deep() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    // The folder passed to `pull_folder` is depth 1, so 32 more nested
    // folders below it reaches depth 33, one past the documented 32 level
    // cap (design/errors.json, Runtime::FolderTooLarge).
    let mut path = phone.shared_root().join("Camera");
    std::fs::create_dir(&path).expect("a folder for the camera roll");
    for _ in 0..32 {
        path.push("Sub");
        std::fs::create_dir(&path).expect("a nested folder");
    }
    std::fs::write(path.join("deep.jpg"), sample_bytes(16)).expect("a file at the bottom");

    let error = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect_err("a folder nested this deep should be refused");
    assert_eq!(code_of_error(&error), "Runtime::FolderTooLarge");
    assert!(
        mac.engine.batches().is_empty(),
        "a refused folder listing creates no batch"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "a refused folder listing creates no transfer"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
fn retry_keeps_a_transfers_batch_id_and_clears_the_batchs_end_time() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(AlwaysMissingFs));
    pair_with_peer(&side, &peer);

    let batch_id = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect("the folder copy is accepted; its one file fails once fetched");

    let engine = Arc::clone(&side.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to fail", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Failed)
    });
    let failed_batch = side
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the failed batch is listed");
    assert!(
        failed_batch.ended_unix_secs.is_some(),
        "a batch with nothing left moving has an end time"
    );

    let transfer_id = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.batch_id.as_deref() == Some(batch_id.as_str()))
        .expect("the one transfer the batch covers")
        .id;

    side.engine
        .retry(transfer_id.clone())
        .expect("a failed transfer can be retried");

    wait_transfer(
        &side,
        &transfer_id,
        "retry to requeue it under the same batch",
        |t| t.state == TransferState::Queued && t.batch_id.as_deref() == Some(batch_id.as_str()),
    );
    let retried_batch = side
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch is still listed");
    assert_eq!(
        retried_batch.ended_unix_secs, None,
        "a batch with something moving again has no end time"
    );

    side.engine.stop();
    peer.close();
}

#[test]
fn a_batch_whose_transfers_all_finished_is_dropped_and_its_file_deleted_at_load() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    std::fs::write(phone.shared_root().join("Camera/a.bin"), sample_bytes(1024))
        .expect("the phone's shared folder should accept a file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Done)
    });

    let batch_file = mac.data.path().join("batches").join(&batch_id);
    assert!(
        batch_file.exists(),
        "the batch record is on disk while the batch is still known"
    );

    mac.engine.stop();
    phone.engine.stop();

    // A new engine on the same folders. The one transfer reached Done, so
    // its own record does not survive: none of this batch's transfer ids
    // name a surviving row.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    assert!(
        engine.batches().iter().all(|b| b.id != batch_id),
        "a batch none of whose transfers survived is dropped at load"
    );
    assert!(
        !batch_file.exists(),
        "its file is removed at load, the same way a device's forgotten batches are"
    );

    engine.stop();
}

/// Item 2 additions, I1: a batch reports its transport and error, and
/// `retry_batch` retries every failed transfer in it.
#[test]
fn a_failed_batch_reports_its_error_and_retry_batch_requeues_it() {
    let side = build("Vamana");

    let unknown = side
        .engine
        .retry_batch("no-such-batch".to_owned())
        .expect_err("an unknown batch id cannot be retried");
    assert_eq!(code_of_error(&unknown), "Runtime::TransferNotFound");

    let peer = start_peer_with(&side.key, Arc::new(AlwaysMissingFs));
    pair_with_peer(&side, &peer);

    let batch_id = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect("the folder copy is accepted; its one file fails once fetched");

    let engine = Arc::clone(&side.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to fail", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Failed)
    });
    let failed_batch = side
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the failed batch is listed");
    assert!(
        failed_batch.error.is_some(),
        "a Failed batch reports the failing transfer's error"
    );

    side.engine
        .retry_batch(batch_id.clone())
        .expect("a batch with a failed transfer can be retried");

    let engine = Arc::clone(&side.engine);
    let wanted = batch_id.clone();
    poll_until("retry_batch to requeue the failed transfer", move || {
        engine.transfers().iter().any(|t| {
            t.batch_id.as_deref() == Some(wanted.as_str()) && t.state == TransferState::Queued
        })
    });

    side.engine.stop();
    peer.close();
}
