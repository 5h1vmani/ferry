//! What `stop` must do, however busy the engine is.
//!
//! Audit 2, findings 1, 2, 4, 6 and 7. A stop in the middle of a transfer,
//! a stop while one side has confirmed and the other has not, a stop that
//! must stop serving files, a peer that accepts and never answers, and the
//! promise that no callback arrives after `stop` returned. The data folder
//! lock is here too, because it is the other way an engine refuses to run.
//!
//! Some tests build a peer by hand, out of `tcp`, `noise`, `rpc` and
//! `frame`. That harness is `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, Side, build, code_of_error, is_code, is_found, loopback_addr, make_engine, mib,
    pair_with_peer, public_key, pull_big, sample_bytes, start_peer, start_peer_with, static_key,
    wait_transfer,
};

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_core::chunk::{ChunkSize, Manifest, manifest_from_bytes};
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind;
use ferry_core::rpc::{Client, FileOps, exchange_hello};
use ferry_core::tcp::{self};
use ferry_runtime::{PairingMethod, TransferState};

/// docs/engine-contract.md item 16c: stop closes every socket, so a worker
/// blocked in a kernel read cannot hold it up.
///
/// A filesystem that accepts the handshake and answers `stat` and
/// `manifest` honestly, but never answers a `read`. Stands in for a peer
/// that accepts a connection and then goes silent mid-transfer.
struct NeverAnswersFs {
    bytes: Vec<u8>,
}

impl FileOps for NeverAnswersFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        Ok(Entry {
            name: "big.bin".to_owned(),
            kind: FileKind::File,
            size: u64::try_from(self.bytes.len()).unwrap_or(0),
            modified_unix_secs: 1_000_000,
        })
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        // Never answers. The serving thread parks here for the rest of the
        // test process's life; nothing needs it to return, since what this
        // test checks is how quickly the engine's own side gives up.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
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

    fn manifest(&self, path: &RemotePath) -> Result<Manifest, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        Ok(manifest_from_bytes(&self.bytes, ChunkSize::one_mebibyte()))
    }
}

/// Finding 1: stop during a transfer.
#[test]
fn stop_returns_while_a_transfer_is_moving() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(8)));
    // Small answers, each after a wait, so the transfer is still running
    // when `stop` is called.
    peer.fs.cap_reads(64 * 1024);
    peer.fs.slow_down(0, Duration::from_millis(20));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the transfer to start moving", |t| {
        t.bytes_done > 0
    });
    // Item 8: speed_bytes_per_sec is measured over the last two seconds of
    // this transfer's own bytes, so it takes a moment to appear.
    wait_transfer(&side, &id, "a per-transfer speed to appear", |t| {
        t.speed_bytes_per_sec.is_some()
    });
    let moving = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the transfer should be listed while it moves");
    assert_eq!(
        moving.state,
        TransferState::Active,
        "a speed is reported only while the state is Active"
    );
    assert!(
        moving.speed_bytes_per_sec.expect("checked above") > 0,
        "a transfer moving real bytes has a nonzero speed"
    );

    let started = Instant::now();
    side.engine.stop();
    let took = started.elapsed();

    let stopped = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the transfer is still listed once stop returns");
    assert_eq!(
        stopped.speed_bytes_per_sec, None,
        "speed_bytes_per_sec is None once the state is no longer Active"
    );

    peer.close();
    // Five seconds, matching the "quick" budget tests/item_16_stop_pairing.rs
    // and tests/item_17_prefetch.rs use for the same kind of check
    // (docs/agent-runs.md rule 14): comfortably under any real timeout this
    // would otherwise wait on, with more room than three seconds for a slow
    // debug build.
    assert!(took < Duration::from_secs(5), "stop took {took:?}");
}

#[test]
fn stop_returns_quickly_when_a_peer_accepts_and_never_answers_a_read() {
    let side = build("Vamana");
    let peer = start_peer_with(
        &side.key,
        Arc::new(NeverAnswersFs {
            bytes: sample_bytes(mib(1)),
        }),
    );
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "hangs.bin");
    wait_transfer(&side, &id, "the pull to start", |t| {
        t.state == TransferState::Active
    });

    let started = Instant::now();
    side.engine.stop();
    let took = started.elapsed();

    // Five seconds: docs/agent-runs.md rule 14, the same margin
    // tests/item_16_stop_pairing.rs and tests/item_17_prefetch.rs give a
    // "must return quickly" check, well under the peer's own 3600 second
    // fake hang this proves was not waited out.
    assert!(
        took < Duration::from_secs(5),
        "stop took {took:?}, expected under five seconds"
    );

    peer.close();
}

/// Finding 2: stop while one side has confirmed and the other has not.
#[test]
fn stop_returns_when_only_one_side_confirmed() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    phone.engine.set_reachable(true);
    phone.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.start_pairing_with(PairingMethod::Code);
    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    mac.engine
        .pick_candidate(format!("wifi:{phone_addr}"))
        .expect("the injected candidate should be pickable");
    mac.inbox.wait_pairing("the Mac's code", is_code);
    phone.inbox.wait_pairing("the phone's code", is_code);

    // Only the Mac confirms. The phone sends no name, so the Mac's name
    // exchange waits, and `stop` used to wait with it.
    mac.engine.confirm_pairing(true);

    let started = Instant::now();
    mac.engine.stop();
    let took = started.elapsed();
    // Five seconds: docs/agent-runs.md rule 14, the same margin
    // tests/item_16_stop_pairing.rs gives this exact "stop mid name
    // exchange" check, well under the ten second deadline that check
    // proves was not waited out.
    assert!(took < Duration::from_secs(5), "stop took {took:?}");
    phone.engine.stop();
}

/// Finding 4: stop must stop serving files.
#[test]
fn stop_stops_serving_a_connected_peer() {
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
    let first = client.read(&path, 0, 5).expect("a paired peer may read");
    assert_eq!(first, b"hello", "the file is served while the engine runs");

    phone.engine.stop();

    // docs/engine-contract.md item 16c: `stop` now closes every registered
    // socket directly, so a call already in flight on this connection meets
    // a closed socket, not the clean `PermissionDenied` `GuardedFs`'s switch
    // used to answer with while the socket stayed open.
    assert!(
        client.read(&path, 0, 5).is_err(),
        "the connection should be gone once stop has closed its socket"
    );
}

/// Finding 7: no callback after stop returned.
#[test]
fn no_callback_arrives_after_stop_returned() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
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

    let Side {
        engine,
        inbox,
        key: _key,
        data: _data,
        shared: _shared,
        download: _download,
    } = phone;
    engine.stop();
    drop(engine);
    let quiet_at = inbox.ticks();

    // The connection ends now. The serving thread wakes, finishes, and used
    // to report a device change into an engine that no longer exists.
    drop(stream);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        inbox.ticks(),
        quiet_at,
        "no callback may arrive after stop returned"
    );
}

/// Finding 6: one engine per data folder, and no record for a device that is
/// not paired.
#[test]
fn a_second_engine_on_one_data_folder_is_refused() {
    let side = build("Vamana");
    let inbox = Arc::new(Inbox::default());
    let second = make_engine(
        "Vamana again",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    );
    let error = second.err().expect("a second engine must be refused");
    assert_eq!(code_of_error(&error), "Runtime::BadConfig");
    side.engine.stop();

    // Once the first engine has stopped, the folder is free again.
    let third = make_engine(
        "Vamana later",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    )
    .expect("the folder is free once the first engine stopped");
    third.stop();
}
