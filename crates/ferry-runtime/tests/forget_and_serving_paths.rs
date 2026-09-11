//! Forget, and the access log that serving writes.
//!
//! Audit 2, finding 3: forget must reach a connection whose `hello` has not
//! arrived. Batch E, item 13, and findings E1 and E8: entries survive a
//! restart, an operation served just before `stop` is still there, a failed
//! operation is never logged, and forget keeps the log it leaves behind.
//!
//! Some tests build a peer by hand. That harness is
//! `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, Peer, Side, build, key_hex, loopback_addr, make_engine, pair_two_engines,
    pair_with_peer, poll_until, public_key, pull_big, sample_bytes, start_peer, static_key,
    wait_transfer,
};

use std::sync::Arc;
use std::time::Duration;

use ferry_core::noise::SecureStream;
use ferry_core::ops::OpError;
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind;
use ferry_core::rpc::{Client, RpcError, exchange_hello};
use ferry_core::tcp::{self};
use ferry_runtime::{AccessVerb, Actor, TransferState, generate_key};

/// Finding E8: access log behaviours the audit found untested.
///
/// Connect a paired peer to `phone` and finish the name exchange, ready to
/// send file operations. Shared by every finding E8 test below.
fn connect_paired_peer<F>(phone: &Side, peer: &Peer<F>) -> Client<SecureStream> {
    let connection = tcp::connect(
        loopback_addr(phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange runs");
    Client::new(stream)
}

/// Finding 3: forget must reach a connection whose hello has not arrived.
#[test]
fn forget_refuses_a_peer_that_has_not_said_hello() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    // The peer dials in and finishes the handshake, then says nothing.
    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;

    // Let the engine's serving thread reach its wait for the name.
    std::thread::sleep(Duration::from_millis(300));

    phone
        .engine
        .forget(key_hex(&peer.key))
        .expect("a paired device can be forgotten");

    // Now the peer speaks. The engine answers the hello, because that
    // exchange was already waiting, and then finds the device is no longer
    // listed and closes the connection. The first operation fails on the
    // closed connection, however late the hello is.
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange still runs");
    let mut client = Client::new(stream);
    let asked = client.read(
        &RemotePath::parse("Root/anything.bin").expect("a valid path"),
        0,
        16,
    );
    match asked {
        Err(RpcError::Frame(_)) => {}
        other => panic!("the connection should be closed, got {other:?}"),
    }
    phone.engine.stop();
}

#[test]
fn forget_removes_the_devices_batch_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    std::fs::write(phone.shared_root().join("Camera/a.bin"), sample_bytes(1024))
        .expect("the phone's shared folder should accept a file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key.clone(), "Root/Camera".to_owned())
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
    assert!(batch_file.exists(), "the batch record should be on disk");

    mac.engine
        .forget(phone_key)
        .expect("a paired device can be forgotten");
    assert!(
        !batch_file.exists(),
        "forget removes the device's batch files under data_dir/batches/"
    );
    assert!(
        mac.engine.batches().is_empty(),
        "the forgotten device's batches must leave the list"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// Batch E, item 13: the access log.
#[test]
fn a_day_file_older_than_30_days_is_pruned_at_start() {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let access_log_dir = data.path().join("access_log");
    std::fs::create_dir_all(&access_log_dir).expect("the access log folder should be makeable");
    // 2020 is always more than 30 days before whenever this test runs.
    let old_day = access_log_dir.join("20200101");
    std::fs::write(&old_day, []).expect("a day file should write");

    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        generate_key().expect("a fresh key pair"),
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    )
    .expect("the engine should build");
    engine.start().expect("the engine should start");

    assert!(
        !old_day.exists(),
        "a day file over 30 days old must be pruned at start"
    );
    engine.stop();
}

#[test]
fn access_log_entries_survive_a_restart() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(16));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the pull to finish", |t| {
        t.state == TransferState::Done
    });

    let before = side.engine.access_log(None, 10);
    let before_read = before
        .iter()
        .find(|e| e.verb == AccessVerb::Read && e.path == "big.bin")
        .expect("the pull should log a Read entry before the restart")
        .clone();

    side.engine.stop();
    peer.close();

    // A new engine on the same folder, as if the app had been restarted.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");
    engine.start().expect("the engine should start");

    let after = engine.access_log(None, 10);
    let after_read = after
        .iter()
        .find(|e| e.id == before_read.id)
        .expect("the same entry should still be there after the restart");
    assert_eq!(
        after_read.bytes, before_read.bytes,
        "the byte count survives the restart"
    );
    assert_eq!(
        after_read.at_unix_secs, before_read.at_unix_secs,
        "the time survives the restart"
    );
    assert_eq!(
        after_read.device_key_hex, before_read.device_key_hex,
        "the device survives the restart"
    );

    engine.stop();
}

/// Finding E1: stop must not lose an entry still pending when it runs.
#[test]
fn an_operation_served_just_before_stop_is_in_the_log_after_a_restart() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange runs");
    let mut client = Client::new(stream);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    client.read(&path, 0, 5).expect("a paired peer may read");

    // No sleep here: the read's entry is still pending in the roll-up, five
    // seconds short of its own idle timeout, and the connection is still
    // open. `stop` must still write it out rather than lose it.
    phone.engine.stop();

    // A new engine on the same folder, as if the app had been restarted.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Pixel 3 XL",
        phone.key.clone(),
        phone.data.path(),
        phone.shared.path(),
        phone.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");
    engine.start().expect("the engine should start");

    let found = engine.access_log(None, 10);
    let entry = found
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .expect("the read served just before stop should still be in the log");
    // Item 13's folder roll-up: a served read is filed under the folder the
    // file sits in, not under the file.
    assert_eq!(entry.path, "Root");
    assert_eq!(entry.bytes, Some(5));

    engine.stop();
}

#[test]
fn set_mtime_is_never_logged() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    client
        .set_mtime(&path, 1_700_000_000)
        .expect("a paired peer may set the mtime of a file it can write");

    // Close the connection so the served side finalises anything it had
    // pending, if there were anything to finalise.
    drop(client);
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        phone.engine.access_log(None, 10).is_empty(),
        "set_mtime always follows a write that is already logged, so it logs nothing of its own"
    );
    phone.engine.stop();
}

#[test]
fn a_failed_operation_is_never_logged() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);

    let missing = RemotePath::parse("Root/missing.bin").expect("a valid path");
    match client.stat(&missing) {
        Err(RpcError::Remote(OpError::NotFound)) => {}
        other => panic!("a stat on a missing file should be refused, got {other:?}"),
    }

    let real = RemotePath::parse("Root/note.txt").expect("a valid path");
    client
        .stat(&real)
        .expect("a stat on a file that exists should succeed");

    drop(client);
    poll_until("the served stat to be logged", || {
        !phone.engine.access_log(None, 10).is_empty()
    });

    let found = phone.engine.access_log(None, 10);
    assert_eq!(
        found.len(),
        1,
        "only the successful stat is logged, not the failed one"
    );
    // Item 13's folder roll-up: a served stat is filed under the folder the
    // file sits in, and the one file it covers is counted.
    assert_eq!(found[0].path, "Root");
    assert_eq!(found[0].files, Some(1));
    phone.engine.stop();
}

#[test]
fn rename_logs_the_destination_not_the_source() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("old.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);
    let from = RemotePath::parse("Root/old.txt").expect("a valid path");
    let to = RemotePath::parse("Root/new.txt").expect("a valid path");
    client
        .rename(&from, &to)
        .expect("a paired peer may rename a file it can write");

    drop(client);
    poll_until("the served rename to be logged", || {
        !phone.engine.access_log(None, 10).is_empty()
    });

    let found = phone.engine.access_log(None, 10);
    let entry = found
        .iter()
        .find(|e| e.verb == AccessVerb::Rename)
        .expect("the rename should be logged");
    assert_eq!(
        entry.path, "Root/new.txt",
        "the destination is logged, not the source"
    );
    phone.engine.stop();
}

#[test]
fn forget_keeps_the_devices_access_log_entries() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    client.read(&path, 0, 5).expect("a paired peer may read");
    drop(client);

    poll_until("the read to be logged before forget", || {
        !phone.engine.access_log(None, 10).is_empty()
    });
    let before = phone.engine.access_log(None, 10);
    assert_eq!(before.len(), 1);

    phone
        .engine
        .forget(key_hex(&peer.key))
        .expect("a paired device can be forgotten");

    let after = phone.engine.access_log(None, 10);
    assert_eq!(
        after.len(),
        1,
        "forget removes the device's peer entry and transfers, not its access log"
    );
    assert_eq!(after[0].id, before[0].id);
    phone.engine.stop();
}
