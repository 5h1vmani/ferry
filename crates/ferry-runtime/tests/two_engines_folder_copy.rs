//! A folder copy over two real engines.
//!
//! `docs/engine-contract.md`, batch D, item 2: `pull_folder` lists a folder
//! with a nested subfolder and groups every file it lands into one batch.

mod common;

use common::engines::{build, pair, sample_bytes};

use std::sync::Arc;

use ferry_runtime::TransferState;

/// `docs/engine-contract.md`, batch D, item 2, end to end: `pull_folder`
/// lists a folder with a nested subfolder, lands every file at the right
/// relative path, and `batches()` reports the aggregates the contract
/// promises once every file is done. Alongside it: a single `pull` names no
/// batch, and an empty folder makes a batch with zero files, already `Done`.
#[test]
// One long, linear narrative, the same choice `two_engines_pair_and_move_a_file`
// makes: every fact this item promises, checked in the order a folder copy
// actually reaches it.
#[allow(clippy::too_many_lines)]
fn pull_folder_groups_its_transfers_into_one_batch() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Camera")).expect("a folder for the camera roll");
    std::fs::create_dir(phone.shared_root.join("Camera/Sub")).expect("a nested subfolder");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    let bytes_c = sample_bytes();
    std::fs::write(phone.shared_root.join("Camera/a.bin"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/b.bin"), &bytes_b)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/Sub/c.bin"), &bytes_c)
        .expect("the phone's shared folder should accept a nested file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key.clone(), "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_batch = batch_id.clone();
    mac.inbox.wait_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted_batch && b.state == TransferState::Done)
    });

    let batch = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the finished batch should still be listed");
    assert_eq!(batch.files_total, 3, "three files were under the folder");
    assert_eq!(batch.files_done, 3, "every file finished");
    assert_eq!(
        batch.bytes_total,
        (bytes_a.len() + bytes_b.len() + bytes_c.len()) as u64,
        "the byte total is the sum over its transfers"
    );
    assert_eq!(batch.state, TransferState::Done);
    assert_eq!(batch.device_key_hex, phone_key);

    let transfers = mac.engine.transfers();
    let in_batch: Vec<&ferry_runtime::TransferInfo> = transfers
        .iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch.id.as_str()))
        .collect();
    assert_eq!(
        in_batch.len(),
        3,
        "every transfer this folder copy made carries the batch id"
    );

    assert_eq!(
        std::fs::read(mac.download_root.join("Camera/a.bin")).expect("a.bin should have landed"),
        bytes_a
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("Camera/b.bin")).expect("b.bin should have landed"),
        bytes_b
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("Camera/Sub/c.bin"))
            .expect("the nested file should have landed at its relative path"),
        bytes_c
    );

    // A single pull, outside any folder copy, names no batch.
    let single_id = mac
        .engine
        .pull(
            phone_key.clone(),
            "Root/Camera/a.bin".to_owned(),
            "alone.bin".to_owned(),
        )
        .expect("a single pull should be accepted");
    let engine = Arc::clone(&mac.engine);
    let wanted_single = single_id.clone();
    mac.inbox.wait_until("the single pull to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_single && t.state == TransferState::Done)
    });
    let single = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == single_id)
        .expect("the single transfer should still be listed");
    assert_eq!(single.batch_id, None, "a single pull has no batch");

    // An empty folder makes a batch with zero files, already `Done`.
    std::fs::create_dir(phone.shared_root.join("Empty")).expect("an empty folder");
    let empty_batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Empty".to_owned())
        .expect("an empty folder copy should be accepted");
    let empty_batch = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == empty_batch_id)
        .expect("the empty batch should be listed");
    assert_eq!(empty_batch.files_total, 0);
    assert_eq!(empty_batch.files_done, 0);
    assert_eq!(empty_batch.state, TransferState::Done);
    assert_eq!(
        empty_batch.ended_unix_secs,
        Some(empty_batch.started_unix_secs),
        "a batch with zero files ends when it starts"
    );

    mac.engine.stop();
    phone.engine.stop();
}
