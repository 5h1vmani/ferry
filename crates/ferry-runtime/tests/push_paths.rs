//! Pushes that go wrong.
//!
//! `docs/engine-contract.md`, item 5, and the H audit: a push cut and
//! resumed, a peer that stores nothing, a peer that claims extra bytes
//! written, two live pushes to one destination, a local file edited between
//! attempts, a push that survives a restart, and the paths a bad local path
//! takes.
//!
//! Some tests build a peer by hand. That harness is
//! `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, MIB, build, code_of_error, key_hex, loopback_addr, make_engine, mib, pair_two_engines,
    pair_with_peer, poll_until, sample_bytes, start_peer, start_peer_with, wait_transfer,
};

use std::sync::Arc;
use std::time::Duration;

use ferry_core::chunk::{ChunkSize, Manifest, manifest_from_bytes};
use ferry_core::ops::{Entry, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::FileOps;
use ferry_runtime::{AccessVerb, Actor, Direction, TransferState};

/// H4: a peer that claims to have written more than it was sent is fatal.
///
/// A peer that answers every write by claiming it wrote more bytes than
/// the call ever sent it. Stands in for a peer that does not speak the
/// write protocol honestly.
struct ClaimsExtraWrittenFs;

impl FileOps for ClaimsExtraWrittenFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
    }

    fn write(&self, _path: &RemotePath, _offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        Ok(u32::try_from(bytes.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1))
    }

    fn truncate(&self, _path: &RemotePath, _length: u64) -> Result<(), OpError> {
        Ok(())
    }

    fn rename(&self, _from: &RemotePath, _to: &RemotePath) -> Result<(), OpError> {
        Ok(())
    }

    fn set_mtime(&self, _path: &RemotePath, _modified_unix_secs: i64) -> Result<(), OpError> {
        Ok(())
    }

    fn mkdir(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn delete(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Ok(manifest_from_bytes(&[], ChunkSize::one_mebibyte()))
    }
}

/// H1: a push that never verifies must fail, not retry forever.
///
/// A peer that answers every write, truncate, rename, and set-mtime with
/// success, but stores nothing: its manifest always reports the file as
/// present and empty, whatever was "written" to it. Stands in for a peer
/// that acknowledges everything and keeps none of it.
struct AcksButStoresNothingFs;

impl FileOps for AcksButStoresNothingFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
    }

    fn write(&self, _path: &RemotePath, _offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        Ok(u32::try_from(bytes.len()).unwrap_or(u32::MAX))
    }

    fn truncate(&self, _path: &RemotePath, _length: u64) -> Result<(), OpError> {
        Ok(())
    }

    fn rename(&self, _from: &RemotePath, _to: &RemotePath) -> Result<(), OpError> {
        Ok(())
    }

    fn set_mtime(&self, _path: &RemotePath, _modified_unix_secs: i64) -> Result<(), OpError> {
        Ok(())
    }

    fn mkdir(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn delete(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        // Always answers as if the file exists and is empty, no matter what
        // was written to it: this peer keeps nothing.
        Ok(manifest_from_bytes(&[], ChunkSize::one_mebibyte()))
    }
}

/// Item 5: a push resumes across a cut connection.
///
/// `docs/engine-contract.md` item 5: a push cut at three points -- early,
/// mid-transfer, and during the final rename -- resumes on its own and
/// rewrites at most one chunk. Proved with wire bytes, the technique
/// `resume_sweep.rs` uses for a pull, but at three points rather than a
/// full sweep: item 5 says the resume rule is the same code path that sweep
/// already proves, so this only has to show a push reaches it too.
///
/// Two real engines, the way `pair_two_engines` sets up for a batch: a
/// pull's own hand-built `Peer` only answers `read`, and a push needs
/// `write`, `truncate`, `rename`, `set_mtime` and the manifest request
/// served for real.
#[test]
fn a_cut_push_resumes_and_rewrites_at_most_one_chunk() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);
    mac.engine.set_backoff(Duration::ZERO);

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let bytes = sample_bytes(mib(3));
    let local_path = source.path().join("big.bin");
    std::fs::write(&local_path, &bytes).expect("the local file should write");
    let local_path_text = local_path.to_string_lossy().into_owned();

    let timed_push = |remote_name: &str| -> u64 {
        let before = mac.engine.wire_bytes();
        let id = mac
            .engine
            .push(
                phone_key.clone(),
                local_path_text.clone(),
                remote_name.to_owned(),
            )
            .expect("the push should be accepted");
        let engine = Arc::clone(&mac.engine);
        let wanted = id.clone();
        poll_until("the push to finish", move || {
            engine
                .transfers()
                .iter()
                .any(|t| t.id == wanted && t.state == TransferState::Done)
        });
        mac.engine.wire_bytes() - before
    };

    let clean = timed_push("Root/clean.bin");
    assert_eq!(
        std::fs::read(phone.shared_root().join("clean.bin")).expect("clean.bin should land"),
        bytes,
        "a clean push must land every byte"
    );

    // One chunk is exactly one mebibyte: `LocalFs::manifest`, on both sides,
    // always builds with `ChunkSize::one_mebibyte()`. The margin past that
    // covers one retry's own hello and manifest exchange, a few hundred
    // bytes at most for a three chunk file.
    let bound = clean + MIB + 200_000;

    for (label, n) in [
        ("early", clean / 20),
        ("mid", clean / 2),
        ("late", clean - 60),
    ] {
        mac.engine.set_cut(n.max(1));
        let name = format!("cut-{label}.bin");
        let used = timed_push(&format!("Root/{name}"));
        let landed = std::fs::read(phone.shared_root().join(&name))
            .unwrap_or_else(|_| panic!("a cut at {label} (n={n}) should still land {name}"));
        assert_eq!(landed, bytes, "a cut at {label} must still land every byte");
        assert!(
            used <= bound,
            "a cut at {label} (n={n}) used {used} wire bytes, the bound is {bound}; \
             more than one chunk must have been rewritten"
        );
    }

    mac.engine.stop();
    phone.engine.stop();
}

/// H1: `push.rs`'s `send` used to retry forever when a chunk it sent from
/// offset 0 never verified, because a peer like this always answers the
/// final manifest check with something that differs. Every attempt starts
/// from offset 0 again, since this peer's manifest never shows anything
/// landed, so the first attempt must already be fatal.
#[test]
fn a_push_to_a_peer_that_stores_nothing_fails_within_a_bounded_number_of_attempts() {
    let mac = build("Vamana");
    let peer = start_peer_with(&mac.key, Arc::new(AcksButStoresNothingFs));
    pair_with_peer(&mac, &peer);
    mac.engine.set_backoff(Duration::from_millis(10));

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    std::fs::write(&local_path, sample_bytes(1024)).expect("the local file should write");

    let id = mac
        .engine
        .push(
            key_hex(&peer.key),
            local_path.to_string_lossy().into_owned(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the push to fail rather than retry forever",
        |t| t.state == TransferState::Failed,
    );

    let info = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is still listed");
    assert_eq!(
        code_of_error(&info.error.expect("a failed transfer carries an error")),
        "TransferError::ChunkFailedVerification"
    );

    mac.engine.stop();
    peer.close();
}

/// H4: `write_all_remote` used to trust a peer's claimed write length past
/// what the call actually sent, which moved its own byte count ahead of
/// what really went out: the next piece was read from the wrong offset in
/// the local file and written to the wrong offset on the peer, bytes in
/// between silently skipped on both sides, caught only much later as a
/// plain `ChunkFailedVerification` once the whole file's manifest no
/// longer matches. A peer that claims more than it was sent must instead
/// fail the push cleanly and immediately, naming what actually went wrong.
#[test]
fn a_peer_that_claims_extra_bytes_written_fails_the_push_cleanly() {
    let mac = build("Vamana");
    let peer = start_peer_with(&mac.key, Arc::new(ClaimsExtraWrittenFs));
    pair_with_peer(&mac, &peer);

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    std::fs::write(&local_path, sample_bytes(1024)).expect("the local file should write");

    let id = mac
        .engine
        .push(
            key_hex(&peer.key),
            local_path.to_string_lossy().into_owned(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the push to fail cleanly rather than panic",
        |t| t.state == TransferState::Failed,
    );

    let info = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is still listed");
    assert_eq!(
        code_of_error(&info.error.expect("a failed transfer carries an error")),
        "OpError::Internal"
    );

    mac.engine.stop();
    peer.close();
}

/// H5: two live pushes to the same destination on one device.
///
/// H5: a second push to a destination a live push on the same device is
/// already sending must be refused, not raced: both would try to land the
/// same name, and the second would waste a full send only to lose.
#[test]
fn a_second_push_to_a_destination_already_in_flight_is_refused() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    let source = tempfile::tempdir().expect("a folder for the files being pushed");
    let path_a = source.path().join("a.bin");
    std::fs::write(&path_a, sample_bytes(mib(1))).expect("a.bin should write");
    let path_b = source.path().join("b.bin");
    std::fs::write(&path_b, sample_bytes(mib(1))).expect("b.bin should write");

    let first = mac
        .engine
        .push(
            phone_key.clone(),
            path_a.to_string_lossy().into_owned(),
            "Root/dest.bin".to_owned(),
        )
        .expect("the first push should be accepted");

    let second = mac.engine.push(
        phone_key.clone(),
        path_b.to_string_lossy().into_owned(),
        "Root/dest.bin".to_owned(),
    );
    assert_eq!(
        second.err().map(|error| code_of_error(&error)),
        Some("Runtime::PushInFlight".to_owned()),
        "a second push to the same destination while the first is in flight must be refused"
    );

    wait_transfer(&mac, &first, "the first push to finish", |t| {
        t.state == TransferState::Done
    });

    // Once the first has finished, the same destination is free again.
    let third = mac
        .engine
        .push(
            phone_key.clone(),
            path_b.to_string_lossy().into_owned(),
            "Root/dest.bin".to_owned(),
        )
        .expect("the same destination is free again once the first push has finished");
    wait_transfer(&mac, &third, "the third push to finish", |t| {
        t.state == TransferState::Done
    });

    mac.engine.stop();
    phone.engine.stop();
}

/// H2: a push's stored record is trusted only while the local file still
/// matches the size and modified time recorded when its manifest was
/// built. Editing the file between two attempts of the same push must
/// land the edited bytes, not the stale ones the first attempt's manifest
/// described.
#[test]
fn editing_the_local_file_between_two_attempts_lands_the_edited_bytes() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);
    mac.engine.set_backoff(Duration::from_secs(2));

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    let original = sample_bytes(mib(3));
    std::fs::write(&local_path, &original).expect("the local file should write");
    let local_path_text = local_path.to_string_lossy().into_owned();

    // Cut partway through the first attempt's sending: well past `build`
    // writing its Record::Ready for the original bytes, and well short of
    // landing the whole file.
    mac.engine.set_cut(MIB);

    let id = mac
        .engine
        .push(
            phone_key.clone(),
            local_path_text.clone(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the cut attempt to fail and wait to retry",
        |t| t.state == TransferState::Paused,
    );

    // H3: the Write entry reflects only what an attempt actually sent, not
    // the whole file up front. The cut attempt above sent at most 1 MiB
    // before it failed, well under the 3 MiB file, so the log must not
    // already show the whole file as written.
    let logged_before_retry: u64 = mac
        .engine
        .access_log(None, 100)
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Write)
        .filter_map(|e| e.bytes)
        .sum();
    assert!(
        logged_before_retry < original.len() as u64,
        "the cut attempt must not log the whole file's size up front, got {logged_before_retry}"
    );

    // Edited between attempts: different content and a different size,
    // neither of which the first attempt's manifest describes any more.
    let edited = sample_bytes(mib(2) + 12_345);
    std::fs::write(&local_path, &edited).expect("the edited file should write");

    wait_transfer(&mac, &id, "the retried push to finish", |t| {
        t.state == TransferState::Done
    });

    assert_eq!(
        std::fs::read(phone.shared_root().join("a.bin")).expect("a.bin should have landed"),
        edited,
        "the push must land the edited bytes, not the stale ones from the first attempt"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// H audit: push behaviours the audit found no test for.
///
/// A push survives a restart the same way a pull's first pass does: the
/// stored record is picked up again on a fresh connection, rather than the
/// person having to start over.
#[test]
fn a_push_survives_a_restart() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);
    // Long enough that the cut attempt below is reliably still Paused, not
    // already retried, by the time the poll below checks for it, even
    // under a loaded test run.
    mac.engine.set_backoff(Duration::from_secs(10));

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    let bytes = sample_bytes(mib(3));
    std::fs::write(&local_path, &bytes).expect("the local file should write");

    // Cut partway through sending, well past `build` writing its
    // Record::Ready.
    mac.engine.set_cut(MIB);

    let id = mac
        .engine
        .push(
            phone_key,
            local_path.to_string_lossy().into_owned(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the cut attempt to fail and wait to retry",
        |t| t.state == TransferState::Paused,
    );

    mac.engine.stop();

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

    let found = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the interrupted push should be listed again");
    assert_eq!(found.state, TransferState::Paused);
    assert_eq!(
        found.direction,
        Direction::Push,
        "a push is still Direction::Push"
    );

    engine.offer_candidate(loopback_addr(&phone));
    engine.start().expect("the engine should start");

    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("the resumed push to finish", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });

    assert_eq!(
        std::fs::read(phone.shared_root().join("a.bin")).expect("a.bin should have landed"),
        bytes
    );

    engine.stop();
    phone.engine.stop();
}

/// `retry_batch` requeues a push the same way it requeues a pull: a batch
/// with a failed transfer accepts the call and the failed row moves back
/// to `Queued`.
#[test]
fn retry_batch_requeues_a_failed_push() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    let source = tempfile::tempdir().expect("a folder for the files being pushed");
    let good_path = source.path().join("good.bin");
    std::fs::write(&good_path, sample_bytes(16)).expect("good.bin should write");
    // Never created: a missing local path fails this one file outright.
    let missing_path = source.path().join("missing.bin");

    let batch_id = mac
        .engine
        .push_files(
            phone_key,
            vec![
                good_path.to_string_lossy().into_owned(),
                missing_path.to_string_lossy().into_owned(),
            ],
            "Root".to_owned(),
        )
        .expect("push_files should be accepted; the missing file fails once attempted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to fail", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Failed)
    });

    mac.engine
        .retry_batch(batch_id.clone())
        .expect("a batch with a failed transfer can be retried");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("retry_batch to requeue the failed push", move || {
        engine.transfers().iter().any(|t| {
            t.batch_id.as_deref() == Some(wanted.as_str()) && t.state == TransferState::Queued
        })
    });

    mac.engine.stop();
    phone.engine.stop();
}

/// A relative local path is refused before anything is queued: there is
/// no root to make it relative to.
#[test]
fn a_relative_local_path_is_refused_before_anything_is_queued() {
    let mac = build("Vamana");
    let peer = start_peer(&mac.key, sample_bytes(16));
    pair_with_peer(&mac, &peer);

    let result = mac.engine.push(
        key_hex(&peer.key),
        "relative/path.bin".to_owned(),
        "Root/a.bin".to_owned(),
    );
    assert_eq!(
        code_of_error(&result.expect_err("a relative path has no root to strip")),
        "OpError::NotFound"
    );

    mac.engine.stop();
    peer.close();
}

/// A missing, a directory, and a symlinked local path each fail the queued
/// push with the matching `OpError`, the same refusals `LocalFs` gives
/// every other operation.
#[test]
fn a_bad_local_path_fails_the_queued_push_with_the_matching_op_error() {
    let mac = build("Vamana");
    let peer = start_peer(&mac.key, sample_bytes(16));
    pair_with_peer(&mac, &peer);

    let source = tempfile::tempdir().expect("a folder for the local paths");
    let real_dir = source.path().join("a-directory");
    std::fs::create_dir(&real_dir).expect("the directory should create");
    let real_file = source.path().join("real.bin");
    std::fs::write(&real_file, sample_bytes(16)).expect("the real file should write");
    #[cfg(unix)]
    let link = source.path().join("a-symlink");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real_file, &link).expect("the symlink should create");

    let cases: Vec<(&str, std::path::PathBuf, &str)> = {
        let mut cases = vec![
            (
                "missing",
                source.path().join("does-not-exist.bin"),
                "OpError::NotFound",
            ),
            ("directory", real_dir, "OpError::IsADirectory"),
        ];
        #[cfg(unix)]
        cases.push(("symlink", link, "OpError::Unsupported"));
        cases
    };

    for (label, local_path, want_code) in cases {
        let id = mac
            .engine
            .push(
                key_hex(&peer.key),
                local_path.to_string_lossy().into_owned(),
                format!("Root/{label}.bin"),
            )
            .unwrap_or_else(|_| panic!("a {label} local path is queued, not refused up front"));
        wait_transfer(&mac, &id, &format!("the {label} push to fail"), |t| {
            t.state == TransferState::Failed
        });
        let info = mac
            .engine
            .transfers()
            .into_iter()
            .find(|t| t.id == id)
            .expect("the failed transfer is still listed");
        assert_eq!(
            code_of_error(&info.error.expect("a failed transfer carries an error")),
            want_code,
            "a {label} local path must fail with {want_code}"
        );
    }

    mac.engine.stop();
    peer.close();
}

/// An empty file pushes and lands like any other: item 5's "an empty file
/// still needs its partial to exist" path.
#[test]
fn pushing_an_empty_file_lands_it() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("empty.bin");
    std::fs::write(&local_path, []).expect("an empty file should still write");

    let id = mac
        .engine
        .push(
            phone_key,
            local_path.to_string_lossy().into_owned(),
            "Root/empty.bin".to_owned(),
        )
        .expect("the push should be accepted");
    wait_transfer(&mac, &id, "the empty push to finish", |t| {
        t.state == TransferState::Done
    });

    assert_eq!(
        std::fs::read(phone.shared_root().join("empty.bin")).expect("empty.bin should have landed"),
        Vec::<u8>::new()
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// A push into a name a file already sits at on the peer replaces it: this
/// is the person's own explicit destination, unlike job 7's automatic
/// copies, which never overwrite anything (G5).
#[test]
fn a_push_over_an_existing_file_on_the_peer_replaces_it() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::write(phone.shared_root().join("a.bin"), b"already here")
        .expect("the existing file should write");

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    let bytes = sample_bytes(1024);
    std::fs::write(&local_path, &bytes).expect("the local file should write");

    let id = mac
        .engine
        .push(
            phone_key,
            local_path.to_string_lossy().into_owned(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");
    wait_transfer(&mac, &id, "the push to finish", |t| {
        t.state == TransferState::Done
    });

    assert_eq!(
        std::fs::read(phone.shared_root().join("a.bin")).expect("a.bin should still be there"),
        bytes,
        "the push replaces the file already at that name"
    );

    mac.engine.stop();
    phone.engine.stop();
}
