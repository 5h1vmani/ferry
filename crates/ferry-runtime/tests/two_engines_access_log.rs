//! The access log over two real engines.
//!
//! `docs/engine-contract.md`, batch E, item 13: each side records what it
//! did, and a folder copy rolls up into one entry rather than one per file.

mod common;

use common::engines::{build, pair, sample_bytes};

use std::sync::Arc;

use ferry_runtime::{AccessVerb, Actor, TransferState};

/// `docs/engine-contract.md`, batch E, item 13, end to end: a listing and a
/// pull each leave a `Peer` entry on the served side and a matching `This`
/// entry on the calling side, the device filter narrows to one device, and
/// `access_log_changed` fires on both engines.
#[test]
fn access_log_records_a_listing_and_a_pull_on_both_sides() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("Photos/holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let entries = mac
        .engine
        .list(phone_key.clone(), "Root/Photos".to_owned())
        .expect("the folder should list");
    assert_eq!(entries.len(), 1, "one file sits under Photos");

    let id = mac
        .engine
        .pull(
            phone_key.clone(),
            "Root/Photos/holiday.bin".to_owned(),
            "holiday.bin".to_owned(),
        )
        .expect("the pull should be accepted");
    let engine = Arc::clone(&mac.engine);
    let wanted_id = id.clone();
    mac.inbox.wait_until("the transfer to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_id && t.state == TransferState::Done)
    });

    // The calling side ("This"): both calls finalise their own entry at
    // once, so nothing here needs to wait.
    let mac_log = mac.engine.access_log(None, 100);
    let this_list = mac_log
        .iter()
        .find(|e| e.actor == Actor::This && e.verb == AccessVerb::List)
        .expect("the calling side should record its own listing");
    assert_eq!(this_list.path, "Root/Photos");
    assert_eq!(this_list.entries, Some(1));

    let this_read = mac_log
        .iter()
        .find(|e| e.actor == Actor::This && e.verb == AccessVerb::Read)
        .expect("the calling side should record its own pull");
    assert_eq!(this_read.path, "Root/Photos/holiday.bin");
    assert_eq!(this_read.bytes, Some(bytes.len() as u64));

    assert!(
        mac.inbox.lock().access_log_ticks > 0,
        "access_log_changed should have fired on the calling side"
    );

    // The served side ("Peer"): the entries finalise once the served
    // connection ends, which happens on the phone's own serving thread, so
    // this polls instead of assuming it has already happened.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record what it served", move || {
            let log = phone_engine.access_log(None, 100);
            log.iter()
                .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::List)
                && log
                    .iter()
                    .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        });
    let phone_log = phone.engine.access_log(None, 100);
    let peer_list = phone_log
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::List)
        .expect("the served side should record the listing");
    assert_eq!(peer_list.entries, Some(1));
    let peer_read = phone_log
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .expect("the served side should record the read");
    assert_eq!(peer_read.bytes, Some(bytes.len() as u64));

    assert!(
        phone.inbox.lock().access_log_ticks > 0,
        "access_log_changed should have fired on the served side"
    );

    // The device filter: an unrelated key hex sees nothing, the real one
    // sees what was just recorded.
    let stranger = "ff".repeat(32);
    assert!(mac.engine.access_log(Some(stranger), 100).is_empty());
    assert!(!mac.engine.access_log(Some(phone_key), 100).is_empty());

    mac.engine.stop();
    phone.engine.stop();
}

/// `docs/engine-contract.md`, item 13, "Rolling up": a file copied as part
/// of a folder copy logs nothing of its own on the calling side, because the
/// folder's own entry, with its file count and byte total, already covers
/// it. The served side knows nothing of batches, so it still logs one entry
/// per file, exactly as an ordinary pull would.
#[test]
fn pull_folder_logs_the_folder_once_not_once_per_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Camera")).expect("a folder for the camera roll");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    std::fs::write(phone.shared_root.join("Camera/a.bin"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/b.bin"), &bytes_b)
        .expect("the phone's shared folder should accept a file");

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

    // The calling side: the folder's own Read entry is the only one, and it
    // is not one of the two files.
    let mac_log = mac.engine.access_log(None, 100);
    let this_reads: Vec<_> = mac_log
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Read)
        .collect();
    assert_eq!(
        this_reads.len(),
        1,
        "the folder entry is the only Read the calling side logs"
    );
    assert_eq!(this_reads[0].path, "Root/Camera");
    assert_eq!(this_reads[0].files, Some(2));
    assert_eq!(
        this_reads[0].bytes,
        Some((bytes_a.len() + bytes_b.len()) as u64),
        "the folder entry's byte total covers both files"
    );

    // The served side does not know about batches, so it records each file
    // it served on its own. Item 13's folder roll-up then files every one
    // of them under the folder they sit in, so no entry names a file and
    // the byte total across them covers both. How many entries there are
    // depends on how many connections the transfer pool used, which is not
    // this test's concern; what each one says is.
    let both_files = (bytes_a.len() + bytes_b.len()) as u64;
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record the bytes it served", move || {
            phone_engine
                .access_log(None, 100)
                .iter()
                .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
                .filter_map(|e| e.bytes)
                .sum::<u64>()
                >= both_files
        });
    let phone_log = phone.engine.access_log(None, 100);
    let peer_reads: Vec<_> = phone_log
        .iter()
        .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .collect();
    assert!(
        !peer_reads.is_empty(),
        "the served side records what it read"
    );
    assert!(
        peer_reads.iter().all(|e| e.path == "Root/Camera"),
        "every served read is filed under the folder, not the file: {peer_reads:?}"
    );
    assert_eq!(
        peer_reads.iter().filter_map(|e| e.bytes).sum::<u64>(),
        both_files,
        "the served reads add up to both files"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// `docs/engine-contract.md`, item 13, "Rolling up": a folder read is one
/// fact, and a write is not.
///
/// Item 17's prefetch reads the head of every image in a folder over one
/// connection. Before this, the served side keyed a roll-up on the
/// connection, the verb, and the whole path, so one folder open left one
/// `read` line per file and a person scrolled past hundreds of them. A
/// `read` and a `stat` are now keyed on the parent folder instead, with
/// `files` counting the distinct files and `bytes` adding up. A `write`
/// keeps its own path, because a person needs to see which file changed.
///
/// Every call below borrows the same pooled connection, the way item 19's
/// own test proves two listings in a row do, so the served side sees one
/// connection for all six.
#[test]
fn reads_of_three_files_in_one_folder_roll_up_but_writes_do_not() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Gallery")).expect("a folder for the photos");
    let names = ["a.bin", "b.bin", "c.bin"];
    let lengths = [10usize, 20, 30];
    for (name, length) in names.iter().zip(lengths) {
        std::fs::write(
            phone.shared_root.join("Gallery").join(name),
            vec![7u8; length],
        )
        .expect("the phone's shared folder should accept a file");
    }

    // The three reads first, then the three writes. A write's own path is
    // not the folder, so the first of them is what finalises the read
    // entry; the reads have to be unbroken for the roll-up to be tested at
    // all.
    for (name, length) in names.iter().zip(lengths) {
        let got = mac
            .engine
            .read_at(phone_key.clone(), format!("Root/Gallery/{name}"), 0, 64)
            .unwrap_or_else(|error| panic!("reading {name} should succeed: {error}"));
        assert_eq!(got.len(), length, "the whole of {name} comes back");
    }
    for name in names {
        mac.engine
            .write_at(
                phone_key.clone(),
                format!("Root/Gallery/{name}"),
                0,
                vec![9u8; 4],
            )
            .unwrap_or_else(|error| panic!("writing {name} should succeed: {error}"));
    }

    // Stopping the Mac closes the pooled connection, which ends the phone's
    // serving thread, which finalises every entry still pending on it. That
    // is faster and surer than waiting out the five second idle rule.
    mac.engine.stop();

    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record every served operation", move || {
            let log = phone_engine.access_log(None, 100);
            log.iter()
                .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Write)
                .count()
                == 3
                && log
                    .iter()
                    .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        });

    let phone_log = phone.engine.access_log(None, 100);
    let peer_reads: Vec<_> = phone_log
        .iter()
        .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .collect();
    assert_eq!(
        peer_reads.len(),
        1,
        "three reads in one folder on one connection are one entry: {peer_reads:?}"
    );
    assert_eq!(peer_reads[0].path, "Root/Gallery");
    assert_eq!(peer_reads[0].files, Some(3), "three distinct files");
    assert_eq!(
        peer_reads[0].bytes,
        Some(lengths.iter().sum::<usize>() as u64),
        "the folder entry's byte total covers all three files"
    );

    let peer_writes: Vec<_> = phone_log
        .iter()
        .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Write)
        .collect();
    assert_eq!(
        peer_writes.len(),
        3,
        "a write keeps its own line, one per file: {peer_writes:?}"
    );
    for name in names {
        assert!(
            peer_writes
                .iter()
                .any(|e| e.path == format!("Root/Gallery/{name}")),
            "the write to {name} names the file it changed"
        );
    }

    phone.engine.stop();
}
