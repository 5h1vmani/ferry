//! The settings a person changes, and what they change.
//!
//! The download folder set before start, and `docs/engine-contract.md`,
//! item 14: turning automatic copying on copies new photos once and never
//! repeats them, a manual pull writes a held row that automatic copying
//! will not repeat, and both survive a restart.
//!
//! Some tests build a peer by hand. That harness is
//! `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, Side, build, code_of_error, key_hex, make_engine, pair_with_peer, poll_until, pull_big,
    sample_bytes, start_peer, start_peer_with, wait_transfer,
};

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ferry_core::memfs::MemoryFs;
use ferry_runtime::{TransferState, generate_key};

/// Item 14: automatic copying.
///
/// A `MemoryFs` peer gives exact control over names, sizes and content,
/// which is what the skip rules turn on. The Mac dials it for `list`, for
/// each `set_auto_copy(true)` while reachable, and for each run that follows
/// from that: `pair_with_peer` alone does not make the Mac consider the peer
/// reachable, the same way it does not in `two_engines.rs`, because the Mac
/// is the side that dialled to pair and only the accepting side's own
/// `mark_reachable` call ever fires from that connection.
///
/// Turn the switch off then on again, the alternative
/// `docs/engine-contract.md` item 14 names to a second reachability
/// transition, and wait for the run it starts to end.
/// The current Unix time, in whole seconds. `last_run_unix_secs` has this
/// same resolution, which is exactly what makes it possible for two runs to
/// share one value: see `toggle_and_wait_for_run`.
fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn toggle_and_wait_for_run(mac: &Side, device: &str, want_files: u32) {
    // G9: `last_run_files` alone can already equal `want_files` from the
    // run before this toggle, most often when two runs in a row both find
    // nothing new. Polling for it alone could then pass before this
    // toggle's own run has even started. `last_run_unix_secs` moving past
    // the value it held before the toggle proves a run actually finished
    // after it.
    let before_unix_secs = mac.engine.auto_copy(device.to_owned()).last_run_unix_secs;

    // A run against the fake peer these tests use can finish inside the
    // same second it started, since `last_run_unix_secs` only has one
    // second of resolution. Waiting here, before the toggle, for the wall
    // clock to move past whatever second `before_unix_secs` was itself
    // read at is what guarantees the run this toggle starts cannot also
    // land in that same second and tie with it forever.
    if let Some(before) = before_unix_secs {
        while current_unix_secs() <= before {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    mac.engine
        .set_auto_copy(device.to_owned(), false)
        .expect("turning the switch off should succeed");
    mac.engine
        .set_auto_copy(device.to_owned(), true)
        .expect("turning it back on should succeed");
    let engine = Arc::clone(&mac.engine);
    let wanted = device.to_owned();
    poll_until(
        "a new run to finish with the expected file count",
        move || {
            let info = engine.auto_copy(wanted.clone());
            info.last_run_unix_secs > before_unix_secs && info.last_run_files == Some(want_files)
        },
    );
}

/// Batch B and C audit, C4: a download folder set before start.
#[test]
fn set_download_dir_before_start_is_where_the_next_pull_lands() {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared_root = tempfile::tempdir().expect("a temporary folder for shared files");
    // Passed to `Config`, but never used: `set_download_dir` below opens a
    // fresher folder before `start` gets the chance to open this one.
    let configured_download = tempfile::tempdir().expect("a temporary folder for the config");
    let real_download = tempfile::tempdir().expect("the folder set_download_dir should open");
    let key = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        key.clone(),
        data.path(),
        shared_root.path(),
        configured_download.path(),
        &inbox,
    )
    .expect("the engine should build from a good config");

    engine
        .set_download_dir(real_download.path().to_string_lossy().into_owned())
        .expect("set_download_dir should accept a good folder before start");
    engine.start().expect("the engine should start");

    let peer = start_peer(&key, sample_bytes(16));
    let side = Side {
        engine,
        inbox,
        key,
        data,
        shared: shared_root,
        download: configured_download,
    };
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the pull to finish", |t| {
        t.state == TransferState::Done
    });

    assert!(
        real_download.path().join("big.bin").exists(),
        "the pull lands in the folder set_download_dir opened before start"
    );
    assert!(
        !side.download_root().join("big.bin").exists(),
        "not in the folder Config named, which start would otherwise have opened"
    );

    side.engine.stop();
    peer.close();
}

#[test]
fn turning_on_automatic_copying_copies_new_photos_and_never_repeats_them() {
    let mac = build("Vamana");
    let fs = Arc::new(MemoryFs::new());
    let bytes_a = sample_bytes(500);
    fs.insert_file("Internal storage/DCIM/a.jpg", bytes_a.clone());
    let peer = start_peer_with(&mac.key, Arc::clone(&fs));
    pair_with_peer(&mac, &peer);
    let device = key_hex(&peer.key);

    mac.engine
        .list(device.clone(), String::new())
        .expect("listing the peer's roots should dial it and succeed");

    let before = mac.engine.auto_copy(device.clone());
    assert!(!before.enabled, "the switch starts off");

    mac.engine
        .set_auto_copy(device.clone(), true)
        .expect("a paired, reachable device should accept the switch");

    let engine = Arc::clone(&mac.engine);
    let wanted = device.clone();
    poll_until("the first run to copy the one file it found", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    let after_first = mac.engine.auto_copy(device.clone());
    assert!(after_first.enabled);
    assert_eq!(after_first.source, "Internal storage/DCIM");
    assert_eq!(
        after_first.destination,
        format!("{}/DCIM", mac.download_root().display())
    );
    assert_eq!(
        std::fs::read(mac.download_root().join("DCIM/a.jpg")).expect("a.jpg should have landed"),
        bytes_a
    );

    // Same device, same file, nothing changed: the next run finds nothing
    // new, and still records that it ran.
    toggle_and_wait_for_run(&mac, &device, 0);

    // The same content at a new path, as a rename on the phone leaves it:
    // skipped by root hash, not copied under its new name.
    fs.insert_file("Internal storage/DCIM/renamed.jpg", bytes_a.clone());
    toggle_and_wait_for_run(&mac, &device, 0);
    assert!(
        !mac.download_root().join("DCIM/renamed.jpg").exists(),
        "content already held under another name is not copied again"
    );

    // A file with new content is copied.
    let bytes_b = sample_bytes(700);
    fs.insert_file("Internal storage/DCIM/b.jpg", bytes_b.clone());
    toggle_and_wait_for_run(&mac, &device, 1);
    assert_eq!(
        std::fs::read(mac.download_root().join("DCIM/b.jpg")).expect("b.jpg should have landed"),
        bytes_b
    );

    mac.engine.stop();
    peer.close();
}

#[test]
fn a_manual_pull_writes_a_held_row_automatic_copying_will_not_repeat() {
    let mac = build("Vamana");
    let fs = Arc::new(MemoryFs::new());
    let bytes_a = sample_bytes(500);
    fs.insert_file("Internal storage/DCIM/a.jpg", bytes_a.clone());
    let bytes_b = sample_bytes(700);
    fs.insert_file("Internal storage/DCIM/b.jpg", bytes_b.clone());
    let peer = start_peer_with(&mac.key, Arc::clone(&fs));
    pair_with_peer(&mac, &peer);
    let device = key_hex(&peer.key);

    // A manual pull, before automatic copying is ever turned on, is the
    // same kind of completed pull a batch's own transfers are.
    let id = mac
        .engine
        .pull(
            device.clone(),
            "Internal storage/DCIM/a.jpg".to_owned(),
            "manual-a.jpg".to_owned(),
        )
        .expect("a manual pull should be accepted");
    wait_transfer(&mac, &id, "the manual pull to finish", |t| {
        t.state == TransferState::Done
    });

    mac.engine
        .set_auto_copy(device.clone(), true)
        .expect("a paired, reachable device should accept the switch");
    let engine = Arc::clone(&mac.engine);
    let wanted = device.clone();
    poll_until("the run to skip the manually pulled file", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    assert!(
        !mac.download_root().join("DCIM/a.jpg").exists(),
        "the manually pulled file is not copied again under DCIM"
    );
    assert_eq!(
        std::fs::read(mac.download_root().join("DCIM/b.jpg")).expect("b.jpg should have landed"),
        bytes_b
    );

    mac.engine.stop();
    peer.close();
}

#[test]
fn automatic_copying_and_the_held_index_survive_a_restart() {
    let mac = build("Vamana");
    let fs = Arc::new(MemoryFs::new());
    let bytes_a = sample_bytes(500);
    fs.insert_file("Internal storage/DCIM/a.jpg", bytes_a.clone());
    let peer = start_peer_with(&mac.key, Arc::clone(&fs));
    pair_with_peer(&mac, &peer);
    let device = key_hex(&peer.key);

    mac.engine
        .list(device.clone(), String::new())
        .expect("listing should succeed");
    mac.engine
        .set_auto_copy(device.clone(), true)
        .expect("the switch should turn on");
    let engine = Arc::clone(&mac.engine);
    let wanted = device.clone();
    poll_until("the first run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    let before = mac.engine.auto_copy(device.clone());
    mac.engine.stop();

    let inbox = Arc::new(Inbox::default());
    let restarted = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");
    restarted.start().expect("the engine should start");

    let after = restarted.auto_copy(device.clone());
    assert_eq!(
        after.enabled, before.enabled,
        "the switch survives a restart"
    );
    assert_eq!(
        after.last_run_unix_secs, before.last_run_unix_secs,
        "the last run time survives a restart"
    );
    assert_eq!(
        after.last_run_files, before.last_run_files,
        "the last run's file count survives a restart"
    );

    // The held index survived too: a fresh run over the same file, once the
    // restarted engine can reach the peer again, finds nothing new.
    restarted.offer_candidate(peer.addr);
    restarted
        .list(device.clone(), String::new())
        .expect("listing should succeed again after the restart");
    restarted
        .set_auto_copy(device.clone(), false)
        .expect("turning the switch off should succeed");
    restarted
        .set_auto_copy(device.clone(), true)
        .expect("turning it back on should succeed");
    let watched = Arc::clone(&restarted);
    let wanted = device.clone();
    poll_until("the post restart run to find nothing new", move || {
        watched.auto_copy(wanted.clone()).last_run_files == Some(0)
    });

    restarted.stop();
    peer.close();
}

#[test]
fn set_auto_copy_on_an_unknown_device_is_not_paired() {
    let mac = build("Vamana");
    let unknown = "ab".repeat(32);

    let error = mac
        .engine
        .set_auto_copy(unknown.clone(), true)
        .expect_err("an unpaired device should be refused");
    assert_eq!(code_of_error(&error), "Runtime::NotPaired");

    let info = mac.engine.auto_copy(unknown);
    assert!(
        !info.enabled,
        "an unknown device always answers as disabled, never as an error"
    );
    mac.engine.stop();
}
