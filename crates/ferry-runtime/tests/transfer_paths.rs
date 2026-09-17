//! Pulls that go wrong, and the records a run leaves behind.
//!
//! Audit 2, findings 5, 10 and 11, and the batch D audit's listing bounds:
//! a first pass cut short and picked up after a restart, a landing folder
//! that is gone, a peer whose cursor never advances, a peer that names an
//! empty entry, a peer that sends a wrong chunk, a peer that answers one
//! byte at a time, and a link that breaks and comes back.
//!
//! Some tests build a peer by hand. That harness is
//! `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, MIB, build, code_of_error, key_hex, make_engine, mib, pair_with_peer, poll_until,
    poll_until_or_describe, pull_big, sample_bytes, start_peer, start_peer_with, wait_transfer,
};

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_core::chunk::{ChunkSize, Manifest, manifest_from_bytes};
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::FileOps;
use ferry_core::session::Transfer;
use ferry_runtime::{Direction, FerryError, TransferState};

/// G10: a `Record::Ready` row whose landing file is missing starts over.
///
/// The bytes of a `Record::Ready` row, in the exact shape `record.rs`
/// writes: format version 3, the ready stage byte, the transfer, then its
/// meta fields. Built by hand because `record.rs`'s own types are private
/// to `ferry-runtime`; this is the same technique other tests here use for
/// a hand-built peer file.
fn encode_ready_record(transfer: &Transfer) -> Vec<u8> {
    let mut e = ferry_core::wire::Encoder::new();
    e.u8(3); // record.rs FORMAT_VERSION
    e.u8(1); // record.rs STAGE_READY
    e.bytes(&transfer.encode());
    // Meta: started_unix_secs, ended_unix_secs (None), direction (Pull),
    // batch_id (None).
    e.fixed(&1_700_000_000i64.to_be_bytes());
    e.u8(0);
    e.u8(0);
    e.u8(0);
    e.finish()
}

/// docs/engine-contract.md item 16a: a first pass verifies every chunk
/// against the peer's manifest as it lands, not only on a later resume.
///
/// A filesystem whose manifest is honest but whose `read` never matches it.
/// Stands in for a peer that serves a correct manifest and then sends the
/// wrong bytes for a chunk.
struct WrongChunkFs {
    bytes: Vec<u8>,
}

impl FileOps for WrongChunkFs {
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

    fn read(&self, path: &RemotePath, _offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        // Every byte answered is wrong, on purpose: the manifest this peer
        // serves is honest, but nothing it reads back ever matches it.
        let want = usize::try_from(length).unwrap_or(0);
        Ok(vec![0xFFu8; want])
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

/// Batch D audit, B2: an entry that names no real child.
///
/// A filesystem whose one `list` entry has an empty name.
struct EmptyNameFs;

impl FileOps for EmptyNameFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: String::new(),
                kind: FileKind::File,
                size: 1,
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
    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Err(OpError::Unsupported)
    }
}

/// Batch D audit, B1: a folder listing that never advances.
///
/// A filesystem whose `list` never lets a folder listing finish: every call
/// answers the same one entry and the same cursor, whatever cursor was
/// asked for. Stands in for a peer that never advances its own paging.
struct StuckListFs;

impl FileOps for StuckListFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: "a.jpg".to_owned(),
                kind: FileKind::File,
                size: 1,
                modified_unix_secs: 1_000_000,
            }],
            Some(0),
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
    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Err(OpError::Unsupported)
    }
}

#[test]
fn a_peer_whose_cursor_never_advances_makes_list_and_pull_folder_refuse_it() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(StuckListFs));
    pair_with_peer(&side, &peer);

    let list_error = side
        .engine
        .list(key_hex(&peer.key), "Camera".to_owned())
        .expect_err("a peer that never advances its cursor should be refused");
    assert_eq!(code_of_error(&list_error), "Runtime::FolderTooLarge");

    let folder_error = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect_err("pull_folder pages the same way and should be refused too");
    assert_eq!(code_of_error(&folder_error), "Runtime::FolderTooLarge");
    assert!(
        side.engine.batches().is_empty(),
        "a refused folder listing creates no batch"
    );
    assert!(
        side.engine.transfers().is_empty(),
        "a refused folder listing creates no transfer"
    );

    peer.close();
}

#[test]
fn pull_folder_fails_cleanly_when_a_peer_names_an_entry_with_an_empty_name() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(EmptyNameFs));
    pair_with_peer(&side, &peer);

    let error = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect_err("an empty entry name should be refused on decode, not queued");
    assert_eq!(code_of_error(&error), "WireError::InvalidPath");
    assert!(
        side.engine.batches().is_empty(),
        "a refused folder listing creates no batch"
    );
    assert!(
        side.engine.transfers().is_empty(),
        "a refused folder listing creates no transfer"
    );

    peer.close();
}

/// Batch B and C audit, C6: a retried transfer clears its end time.
#[test]
fn retry_clears_the_transfers_end_time() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(16));
    pair_with_peer(&side, &peer);

    // `ScriptedFs::stat` answers `NotFound` for anything but "big.bin", so
    // this fails at once, the peer having refused the very first call.
    let id = side
        .engine
        .pull(
            key_hex(&peer.key),
            "missing.bin".to_owned(),
            "missing.bin".to_owned(),
        )
        .expect("the pull is accepted; the peer refuses it once dialled");
    wait_transfer(&side, &id, "the pull to fail", |t| {
        t.state == TransferState::Failed
    });
    let failed = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is listed");
    assert!(
        failed.ended_unix_secs.is_some(),
        "a failed transfer has an end time"
    );

    side.engine
        .retry(id.clone())
        .expect("a failed transfer can be retried");
    // Caught the moment retry has cleared the row and requeued it, before
    // the same refusal fails it again.
    wait_transfer(&side, &id, "retry to clear the end time", |t| {
        t.state == TransferState::Queued && t.ended_unix_secs.is_none()
    });

    side.engine.stop();
    peer.close();
}

/// Finding 5: an interrupted first pass survives a restart.
#[test]
// One long, linear narrative is the point of this test: a transfer cut
// short, restarted, and checked at each stage. Splitting it into smaller
// functions would hide that it is one path, not several.
#[allow(clippy::too_many_lines)]
fn an_interrupted_first_pass_resumes_after_a_restart() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(8)));
    // The first two chunks arrive at once. Everything after them crawls, so
    // the test can stop the engine inside the third chunk. The same crawl
    // continues through the resume below: four reads at this cap, each
    // paused this long, take about three seconds for the first resumed
    // chunk, comfortably past the two second window item 8's speed is
    // measured over.
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(2 * MIB, Duration::from_millis(750));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "two chunks to arrive", |t| {
        t.bytes_done >= 2 * MIB
    });
    let before_stop = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the transfer should be listed before stop");
    assert_eq!(
        before_stop.direction,
        Direction::Pull,
        "a pull is always Direction::Pull until item 5"
    );
    assert_eq!(
        before_stop.ended_unix_secs, None,
        "a transfer still moving has no end time"
    );
    side.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    // The link stays slow a little longer: item 8's speed is checked below,
    // right after the resume starts, while it is still measurable.
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

    let found = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the interrupted transfer should be listed again");
    assert_eq!(
        found.state,
        TransferState::Paused,
        "an interrupted transfer comes back paused"
    );
    assert_eq!(
        found.bytes_done,
        2 * MIB,
        "it comes back at the last verified chunk"
    );
    assert_eq!(found.bytes_total, 8 * MIB, "it knows the size of the file");
    assert_eq!(
        found.started_unix_secs, before_stop.started_unix_secs,
        "the start time survives the restart, item 9's whole point"
    );
    assert_eq!(
        found.ended_unix_secs, None,
        "a record this crate writes never carries an end time"
    );
    assert_eq!(
        found.direction,
        Direction::Pull,
        "a restarted transfer is still a pull"
    );
    // Item 7: 8 MiB at a 1 MiB chunk size is 8 chunks, and 2 MiB verified in
    // place is exactly 2 whole chunks, so both numbers land on an exact
    // count rather than needing to round.
    assert_eq!(found.chunks_total, 8, "8 MiB at 1 MiB chunks is 8 chunks");
    assert_eq!(
        found.chunks_verified, 2,
        "2 MiB verified in place is 2 whole chunks"
    );

    // The peer is reachable again, so the transfer finishes on its own.
    engine.offer_candidate(peer.addr);
    engine.start().expect("the engine should start");

    // C2: the resume starts from 2 MiB already verified. The link is still
    // the slow one set up above, so the first newly moved chunk takes about
    // three seconds, long enough for a speed to be reported. Before the fix
    // the window started at zero bytes, so this first speed would count the
    // 2 MiB baseline as if it had just moved, several times faster than the
    // link this test allows. It is checked here, before `speed_up` below
    // lets the rest of the file arrive at once.
    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("a speed for the resumed transfer to appear", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.speed_bytes_per_sec.is_some())
    });
    let resumed_speed = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .and_then(|t| t.speed_bytes_per_sec)
        .expect("checked by the poll above");
    assert!(
        resumed_speed < 500_000,
        "the resumed transfer's first speed should reflect only new bytes, not the \
         2 MiB baseline too; got {resumed_speed} bytes/sec"
    );

    peer.fs.speed_up();
    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("the transfer to finish", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });
    let landed = std::fs::read(side.download_root().join("big.bin")).expect("the file should land");
    assert_eq!(landed, sample_bytes(mib(8)), "every byte must match");

    let done = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the finished transfer is still listed");
    assert_eq!(
        done.started_unix_secs, before_stop.started_unix_secs,
        "the start time is the same one throughout the transfer's life"
    );
    let ended = done
        .ended_unix_secs
        .expect("a done transfer has an end time");
    assert!(
        ended >= done.started_unix_secs,
        "the end time is not before the start time"
    );
    assert_eq!(
        done.chunks_verified, done.chunks_total,
        "every chunk is verified once the transfer is Done"
    );

    engine.stop();
    peer.close();
}

/// G10: a `Record::Ready` row's fixed temporary name lives under its
/// destination's own folder. `verify_and_land` used to trust that folder
/// was still the one an earlier first pass made it in. If the download
/// folder changed since -- or that folder was simply removed by hand --
/// the next attempt found no parent to write into and failed for good,
/// rather than recreating it and fetching the file again from nothing:
/// exactly what "the manifest is a hint, the disk is the truth"
/// (docs/protocol.md section 9) already means for a file that is not
/// merely short, but missing outright.
///
/// The row here is built by hand rather than produced by a real
/// interrupted transfer, because nothing stops between the moment a real
/// first pass finishes and the same attempt landing the file: both run on
/// the same connection, one call apart. A stored record with a manifest
/// but no fixed temporary name behind it is exactly what an app killed in
/// that narrow window would leave, so this is that state, not a
/// contrivance.
#[test]
fn a_ready_record_whose_landing_folder_is_gone_lands_in_the_current_one() {
    let side = build("Vamana");
    let bytes = sample_bytes(mib(2));
    let peer = start_peer(&side.key, bytes.clone());
    pair_with_peer(&side, &peer);
    let device = key_hex(&peer.key);
    side.engine.stop();

    // A nested destination, not a flat one, so the missing "Camera" folder
    // itself -- not just a missing file at the download root -- is what
    // the fix must recreate.
    let manifest = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    let source = RemotePath::parse("big.bin").expect("a valid path");
    let destination = RemotePath::parse("Camera/big.bin").expect("a valid path");
    let transfer =
        Transfer::new(manifest, source, destination).expect("a fresh transfer should build");
    let id = format!("{device}-g10manual");
    std::fs::write(
        side.data.path().join("transfers").join(format!("{id}.bin")),
        encode_ready_record(&transfer),
    )
    .expect("the hand-built record should write");

    // A fresh engine on the same data and shared folders, but a brand new
    // download folder: "the download folder changed between two
    // attempts".
    let new_download = tempfile::tempdir().expect("a new download folder");
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        new_download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let found = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the hand-built record should be listed");
    assert_eq!(
        found.state,
        TransferState::Paused,
        "a loaded record comes back paused"
    );

    engine.offer_candidate(peer.addr);
    engine.start().expect("the engine should start");

    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("the transfer to land from nothing", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });

    assert_eq!(
        std::fs::read(new_download.path().join("Camera/big.bin"))
            .expect("the file should land in the new download folder"),
        bytes,
        "every byte must match, fetched fresh since nothing was on disk"
    );

    engine.stop();
    peer.close();
}

#[test]
fn a_record_for_a_device_that_is_not_paired_is_dropped() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(4)));
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(MIB, Duration::from_millis(500));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "one chunk to arrive", |t| t.bytes_done >= MIB);
    side.engine.stop();
    peer.close();

    // The device list is lost, the record is not. This is the shape a second
    // engine on one folder used to leave behind.
    std::fs::remove_file(side.data.path().join("peers.bin")).expect("the list should be there");

    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    )
    .expect("the engine should build");
    engine.start().expect("the engine should start");
    assert!(
        engine.transfers().is_empty(),
        "a record whose device is not paired must be dropped"
    );
    let left: Vec<String> = std::fs::read_dir(side.data.path().join("transfers"))
        .expect("the transfers folder should be readable")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.is_empty(), "its record file goes too, found {left:?}");
    engine.stop();
}

#[test]
fn a_peer_that_sends_a_wrong_chunk_in_the_first_pass_fails_verification() {
    let side = build("Vamana");
    let peer = start_peer_with(
        &side.key,
        Arc::new(WrongChunkFs {
            bytes: sample_bytes(mib(2)),
        }),
    );
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "wrong.bin");
    wait_transfer(&side, &id, "the transfer to fail verification", |t| {
        t.state == TransferState::Failed
    });

    let info = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is still listed");
    let error = info.error.expect("a failed transfer carries an error");
    assert_eq!(
        code_of_error(&error),
        "TransferError::ChunkFailedVerification"
    );
    let FerryError::Failed { detail, .. } = error;
    assert_eq!(
        detail.as_deref(),
        Some("0"),
        "the first chunk is the one that fails, and its index travels as the detail"
    );
    assert!(
        !side.download_root().join("wrong.bin").exists(),
        "nothing that fails verification is ever landed"
    );

    side.engine.stop();
    peer.close();
}

/// Finding 10: a few threads, and callbacks that are rationed.
#[test]
fn many_pulls_share_a_few_threads_and_few_callbacks() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(16));
    pair_with_peer(&side, &peer);
    // Nothing answers from here on, so every attempt fails and retries.
    peer.close();

    let ticks_before = side.inbox.ticks();
    let started = Instant::now();
    for i in 0..200 {
        drop(pull_big(&side, &peer, &format!("copy-{i}.bin")));
    }
    let ticks = side.inbox.ticks() - ticks_before;
    let took = started.elapsed();
    assert!(
        ticks < 20,
        "200 pulls in {took:?} produced {ticks} callbacks"
    );
    assert_eq!(side.engine.transfers().len(), 200, "all of them are listed");
    let workers = side.engine.transfer_workers();
    assert!(
        workers <= 4,
        "200 pulls must not become 200 threads, {workers} workers ran"
    );
    side.engine.stop();
}

/// Finding 11: a peer that answers one byte at a time.
#[test]
fn a_peer_that_answers_one_byte_at_a_time_does_not_hold_the_transfer() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(4)));
    peer.fs.cap_reads(1);
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    let engine = Arc::clone(&side.engine);
    let wanted = id.clone();
    let describing_peer = peer.fs.clone();
    // The shared budget, not a hand-rolled one: one byte per read means a
    // real socket round trip per byte, and how many of those it takes to
    // trip the too-slow rule is not this test's business to guess a tight
    // number for (docs/agent-runs.md rule 14).
    poll_until_or_describe(
        "the transfer to leave Active",
        move || {
            engine
                .transfers()
                .iter()
                .any(|t| t.id == wanted && t.state == TransferState::Paused)
        },
        move || format!("{} reads so far", describing_peer.reads()),
    );
    side.engine.stop();
    peer.close();
}

/// The acceptance paths: a broken link, and a device that is not reachable.
#[test]
fn a_transfer_pauses_when_the_link_breaks_and_finishes_when_it_returns() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(4)));
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(MIB, Duration::from_millis(300));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the first chunk to arrive", |t| {
        t.bytes_done >= MIB
    });

    // Break the link, the way a phone leaving the network breaks it.
    peer.cut();
    wait_transfer(&side, &id, "the transfer to pause", |t| {
        t.state == TransferState::Paused
    });

    // Bring it back, and let the retry loop find it.
    peer.fs.speed_up();
    peer.mend();
    wait_transfer(&side, &id, "the transfer to finish", |t| {
        t.state == TransferState::Done
    });
    let landed = std::fs::read(side.download_root().join("big.bin")).expect("the file should land");
    assert_eq!(landed, sample_bytes(mib(4)), "every byte must match");
    side.engine.stop();
    peer.close();
}
