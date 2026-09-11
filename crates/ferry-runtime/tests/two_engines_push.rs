//! Push over two real engines.
//!
//! `docs/engine-contract.md`, item 5: a pushed file lands on the peer under
//! the right root, a push into a read-only root is refused, and `push_files`
//! makes one batch and lands every file in it.

mod common;

use common::engines::{build, code_of_error, pair, sample_bytes};

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ferry_runtime::{AccessVerb, Actor, Direction, Root, TransferState};

/// `docs/engine-contract.md`, item 5, the happy path: a pushed file lands on
/// the peer under the right root, with the right bytes and the right
/// modified time, the row shows `Direction::Push` and ends `Done`, the
/// peer's access log shows the writes, and the sender's shows one `Write`
/// entry for the whole file.
#[test]
fn a_pushed_file_lands_on_the_peer_and_the_row_and_logs_show_it() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    // The file being pushed lives outside every served root, the way a real
    // file the person picks in a panel would.
    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("holiday.bin");
    let bytes = sample_bytes();
    std::fs::write(&local_path, &bytes).expect("the local file should write");
    let old_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&local_path)
        .expect("the local file should reopen")
        .set_modified(old_mtime)
        .expect("the local file's modified time should be settable");

    let id = mac
        .engine
        .push(
            phone_key.clone(),
            local_path.to_string_lossy().into_owned(),
            "Root/holiday.bin".to_owned(),
        )
        .expect("the push should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_id = id.clone();
    mac.inbox.wait_until("the push to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_id && t.state == TransferState::Done)
    });

    let row = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the finished push should still be listed");
    assert_eq!(row.direction, Direction::Push, "a push is Direction::Push");
    assert_eq!(row.state, TransferState::Done);

    let landed_path = phone.shared_root.join("holiday.bin");
    let landed = std::fs::read(&landed_path).expect("the file should be under the phone's root");
    assert_eq!(landed, bytes, "every byte must match");

    let landed_mtime = std::fs::metadata(&landed_path)
        .expect("the landed file should have metadata")
        .modified()
        .expect("the platform should report a modified time")
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let wanted_mtime = old_mtime
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    assert_eq!(
        landed_mtime, wanted_mtime,
        "the pushed file keeps the local file's modified time"
    );

    let leftovers: Vec<String> = std::fs::read_dir(&phone.shared_root)
        .expect("the phone's shared folder should be readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".ferry-part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no partial file should be left behind, found {leftovers:?}"
    );

    // The peer's own access log shows the writes it served, as actor Peer.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record the writes it served", move || {
            phone_engine
                .access_log(None, 100)
                .iter()
                .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Write)
        });

    // The sender's own access log: one Write entry for the one pushed file.
    let mac_log = mac.engine.access_log(None, 100);
    let this_writes: Vec<_> = mac_log
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Write)
        .collect();
    assert_eq!(
        this_writes.len(),
        1,
        "one Write entry for the one pushed file"
    );
    assert_eq!(this_writes[0].path, "Root/holiday.bin");
    assert_eq!(this_writes[0].bytes, Some(bytes.len() as u64));

    mac.engine.stop();
    phone.engine.stop();
}

/// A push into a root the peer marked not writable fails with the same
/// `PermissionDenied` a read-only root already refuses everything else with.
#[test]
fn a_push_into_a_read_only_root_fails_with_permission_denied() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    phone
        .engine
        .set_roots(vec![Root {
            name: "Root".to_owned(),
            path: phone.shared_root.to_string_lossy().into_owned(),
            writable: false,
        }])
        .expect("the phone should accept its own root marked read-only");

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("holiday.bin");
    std::fs::write(&local_path, sample_bytes()).expect("the local file should write");

    let id = mac
        .engine
        .push(
            phone_key,
            local_path.to_string_lossy().into_owned(),
            "Root/holiday.bin".to_owned(),
        )
        .expect("the push should be accepted; the refusal is the peer's, not the call's");

    let engine = Arc::clone(&mac.engine);
    let wanted_id = id.clone();
    mac.inbox.wait_until("the push to fail", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_id && t.state == TransferState::Failed)
    });

    let row = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed push should still be listed");
    let error = row.error.expect("a failed row carries an error");
    assert_eq!(code_of_error(&error), "OpError::PermissionDenied");
    assert!(
        !phone.shared_root.join("holiday.bin").exists(),
        "nothing lands on a root that refused the write"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// `push_files` makes one batch and lands every file, the push mirror of
/// `pull_folder_groups_its_transfers_into_one_batch`.
#[test]
fn push_files_makes_one_batch_and_lands_every_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Uploads"))
        .expect("a folder on the phone to push into");

    let source = tempfile::tempdir().expect("a folder for the files being pushed");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    let bytes_c = sample_bytes();
    let path_a = source.path().join("a.bin");
    let path_b = source.path().join("b.bin");
    let path_c = source.path().join("c.bin");
    std::fs::write(&path_a, &bytes_a).expect("a.bin should write");
    std::fs::write(&path_b, &bytes_b).expect("b.bin should write");
    std::fs::write(&path_c, &bytes_c).expect("c.bin should write");

    let batch_id = mac
        .engine
        .push_files(
            phone_key,
            vec![
                path_a.to_string_lossy().into_owned(),
                path_b.to_string_lossy().into_owned(),
                path_c.to_string_lossy().into_owned(),
            ],
            "Root/Uploads".to_owned(),
        )
        .expect("push_files should be accepted");

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
    assert_eq!(batch.files_total, 3, "three files were pushed");
    assert_eq!(batch.files_done, 3, "every file finished");
    assert_eq!(batch.direction, Direction::Push);
    assert_eq!(
        batch.bytes_total,
        (bytes_a.len() + bytes_b.len() + bytes_c.len()) as u64,
        "the byte total is the sum over its transfers"
    );

    assert_eq!(
        std::fs::read(phone.shared_root.join("Uploads/a.bin")).expect("a.bin should have landed"),
        bytes_a
    );
    assert_eq!(
        std::fs::read(phone.shared_root.join("Uploads/b.bin")).expect("b.bin should have landed"),
        bytes_b
    );
    assert_eq!(
        std::fs::read(phone.shared_root.join("Uploads/c.bin")).expect("c.bin should have landed"),
        bytes_c
    );

    let transfers = mac.engine.transfers();
    let in_batch: Vec<&ferry_runtime::TransferInfo> = transfers
        .iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch.id.as_str()))
        .collect();
    assert_eq!(
        in_batch.len(),
        3,
        "every transfer this push_files call made carries the batch id"
    );

    // H3: one Write entry for the whole folder, logged once the batch
    // ends, with the copied count and bytes -- not one entry per file, and
    // not logged up front before anything had actually landed.
    let mac_log = mac.engine.access_log(None, 100);
    let this_writes: Vec<_> = mac_log
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Write)
        .collect();
    assert_eq!(
        this_writes.len(),
        1,
        "one Write entry for the whole folder, not one per file"
    );
    assert_eq!(this_writes[0].path, "Root/Uploads");
    assert_eq!(
        this_writes[0].bytes,
        Some((bytes_a.len() + bytes_b.len() + bytes_c.len()) as u64)
    );
    assert_eq!(this_writes[0].files, Some(3));

    mac.engine.stop();
    phone.engine.stop();
}
