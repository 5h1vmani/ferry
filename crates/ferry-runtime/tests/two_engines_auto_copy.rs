//! Automatic copying, end to end over two real engines.
//!
//! `docs/engine-contract.md`, item 14: turning the switch on for a device
//! copies that device's camera folder once, and a second reachability
//! transition during a moving batch starts no second run.

mod common;

use common::engines::{build, pair, sample_bytes};

use std::sync::Arc;

use ferry_runtime::Origin;

/// `docs/engine-contract.md`, item 14, over the real named-root system
/// `pull_folder`'s own test above already proves: turning the switch on for
/// a device that is already reachable copies its whole `DCIM` folder once,
/// and turning it off then on again, with nothing new on the phone, copies
/// nothing and still records that the run happened.
#[test]
fn turning_on_automatic_copying_copies_the_camera_folder_once() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("DCIM")).expect("a folder for the camera roll");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    std::fs::write(phone.shared_root.join("DCIM/a.jpg"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("DCIM/b.jpg"), &bytes_b)
        .expect("the phone's shared folder should accept a file");

    // Nothing has dialed the phone yet, so the Mac does not yet consider it
    // reachable. A listing is the cheapest real operation that dials it.
    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("listing the phone's roots should succeed");

    let before = mac.engine.auto_copy(phone_key.clone());
    assert!(!before.enabled, "the switch starts off");

    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("the device is paired, so the switch should turn on");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until("the first run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(2)
    });

    let after_first_run = mac.engine.auto_copy(phone_key.clone());
    assert!(after_first_run.enabled);
    assert_eq!(after_first_run.source, "Root/DCIM");
    assert_eq!(
        after_first_run.destination,
        format!("{}/DCIM", mac.download_root.display())
    );
    assert!(after_first_run.last_run_unix_secs.is_some());
    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/a.jpg")).expect("a.jpg should have landed"),
        bytes_a
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/b.jpg")).expect("b.jpg should have landed"),
        bytes_b
    );

    // Turning the switch off and back on again is the alternative
    // `docs/engine-contract.md` item 14 names to a second reachability
    // transition, for proving the same files are never copied twice.
    mac.engine
        .set_auto_copy(phone_key.clone(), false)
        .expect("turning the switch off should succeed");
    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("turning it back on should succeed");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until("the second run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(0)
    });
    // G8, docs/engine-contract.md item 14: "a run that finds nothing new
    // records a run and makes no batch; the Running state is running."
    // With no batch ever queued for this run, running must be false.
    assert!(
        !mac.engine.auto_copy(phone_key.clone()).running,
        "a run that finds nothing new and makes no batch is not running"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// G2: `maybe_spawn_run`'s guard used to release a device's run slot the
/// moment `run` returned, right after the batch was queued, not once the
/// batch it queued actually finished moving files. `set_auto_copy(true)`
/// is itself one of item 14's three triggers, so calling it a second time
/// while the first run's batch is still moving is a legitimate second
/// trigger, not a test artifact, and it must start no second run.
#[test]
fn a_second_reachability_transition_during_a_moving_batch_starts_no_second_run() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("DCIM")).expect("a folder for the camera roll");
    // Large enough that the pull it starts is still moving by the time the
    // test polls for it and fires the second trigger: many sequential
    // one-mebibyte chunk reads over loopback take measurably longer than
    // one poll tick, unlike a file so small the whole batch could finish
    // before the test ever observes it in flight.
    let big = vec![7u8; 48 * 1024 * 1024];
    std::fs::write(phone.shared_root.join("DCIM/big.bin"), &big)
        .expect("the phone's shared folder should accept a file");

    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("listing the phone's roots should succeed");

    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("the device is paired, so the switch should turn on");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until(
        "the automatic batch to appear and still be moving",
        move || {
            engine.batches().iter().any(|b| {
                b.device_key_hex == wanted
                    && b.origin == Origin::Automatic
                    && b.ended_unix_secs.is_none()
            })
        },
    );
    // G8: `running` is derived from exactly the batch state just polled
    // for above, so it must already agree with it.
    assert!(
        mac.engine.auto_copy(phone_key.clone()).running,
        "a batch that is not Done or Failed means running is true"
    );

    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("turning it on again while it is already on should still succeed");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until("the run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    assert!(
        !mac.engine.auto_copy(phone_key.clone()).running,
        "running is false again once the batch has ended"
    );

    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/big.bin")).expect("big.bin should have landed"),
        big,
        "the file lands whole and exactly once"
    );

    let automatic_batches = mac
        .engine
        .batches()
        .into_iter()
        .filter(|b| b.device_key_hex == phone_key && b.origin == Origin::Automatic)
        .count();
    assert_eq!(
        automatic_batches, 1,
        "the second trigger must not have queued a second batch"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// G5: job 7 says automatic copying is one way and never writes back. A
/// file already sitting at the destination auto-copy would otherwise use,
/// put there by anything other than a held pull of this same file, must
/// never be overwritten: the new file lands beside it under a free name.
#[test]
fn a_destination_already_occupied_lands_beside_it_under_a_free_name() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("DCIM")).expect("a folder for the camera roll");
    let incoming = sample_bytes();
    std::fs::write(phone.shared_root.join("DCIM/a.jpg"), &incoming)
        .expect("the phone's shared folder should accept a file");

    // Nothing this device ever pulled: dragged in by hand, or left over
    // from before this device was ever paired. The held index knows
    // nothing about it.
    std::fs::create_dir(mac.download_root.join("DCIM")).expect("the download folder's DCIM");
    let already_there = b"not something ferry ever put here".to_vec();
    std::fs::write(mac.download_root.join("DCIM/a.jpg"), &already_there)
        .expect("the pre-existing file should write");

    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("listing the phone's roots should succeed");
    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("the device is paired, so the switch should turn on");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until("the run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });

    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/a.jpg"))
            .expect("the pre-existing file must still be there"),
        already_there,
        "the file already at the destination must never be overwritten"
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/a (2).jpg"))
            .expect("the new file should land beside it under a free name"),
        incoming
    );

    mac.engine.stop();
    phone.engine.stop();
}
