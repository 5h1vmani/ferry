//! Two engines in one process, pairing and moving a file.
//!
//! This is the happy path of the acceptance gate. It runs the phone's side
//! and the Mac's side of every step that works: pairing with a code both
//! sides show, a device list that survives the pairing, a real file pulled
//! across a real socket, a forget that clears the device and its records,
//! and a stop that returns quickly.
//!
//! No test sleeps and hopes. Every wait is a condition variable with a
//! generous deadline, so a slow machine makes the test slower, never flaky.
//!
//! Everything that goes wrong lives in the `engine_paths` files. Neither
//! covers USB, which needs `adb`, a cable and a phone, nor real mDNS,
//! because a test machine may sit on a network that refuses multicast.
//! Both are in `docs/manual-checks.md`.

mod common;

use common::engines::{
    assert_expires_about_two_minutes_out, build, build_as, code_of_error, is_code, is_confirmed,
    is_found, loopback_addr, sample_bytes,
};

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_runtime::{DeviceKind, PairingMethod, PairingState, TransferState, Transport};

fn is_waiting(state: &PairingState) -> bool {
    matches!(state, PairingState::Waiting { .. })
}

/// The six digits, or a panic saying what arrived instead.
fn code_of(state: &PairingState) -> String {
    match state {
        PairingState::Code { code, .. } => code.clone(),
        other => panic!("expected a code, got {other:?}"),
    }
}

#[test]
// One long, linear narrative is the point of this test: every step of the
// happy path, in the order a person walks through it. Splitting it into
// smaller functions would hide that it is one path, not several.
#[allow(clippy::too_many_lines)]
fn two_engines_pair_and_move_a_file() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);

    // The phone is the side that waits. The Mac is the side that looks.
    phone.engine.set_reachable(true);
    phone.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.start_pairing_with(PairingMethod::Code);

    // Item 10: every pairing state that has a deadline states it, including
    // Waiting, the state a side that is not looking for anyone starts in.
    let waiting = phone
        .inbox
        .wait_pairing("the phone to wait for pairing", is_waiting);
    let PairingState::Waiting {
        expires_unix_secs: waiting_expires,
    } = waiting
    else {
        panic!("expected Waiting, got {waiting:?}");
    };
    assert_expires_about_two_minutes_out(waiting_expires);

    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);

    let found = mac
        .inbox
        .wait_pairing("the Mac to list a candidate", is_found);
    let PairingState::Found {
        candidates,
        expires_unix_secs: found_expires,
    } = &found
    else {
        panic!("expected candidates, got {found:?}");
    };
    assert_expires_about_two_minutes_out(*found_expires);
    let wanted = format!("wifi:{phone_addr}");
    let chosen = candidates
        .iter()
        .find(|candidate| candidate.id == wanted)
        .unwrap_or_else(|| panic!("the injected candidate {wanted} should be listed"));
    mac.engine
        .pick_candidate(chosen.id.clone())
        .expect("the candidate should be pickable");

    let mac_code_state = mac.inbox.wait_pairing("the Mac to show a code", is_code);
    let phone_code_state = phone
        .inbox
        .wait_pairing("the phone to show a code", is_code);
    for state in [&mac_code_state, &phone_code_state] {
        let PairingState::Code {
            expires_unix_secs, ..
        } = state
        else {
            panic!("expected a code, got {state:?}");
        };
        assert_expires_about_two_minutes_out(*expires_unix_secs);
    }
    let mac_code = code_of(&mac_code_state);
    let phone_code = code_of(&phone_code_state);
    assert_eq!(
        mac_code, phone_code,
        "both screens must show the same six digits"
    );
    assert_eq!(mac_code.len(), 6, "the code is six digits");

    mac.engine.confirm_pairing(true);
    phone.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    assert_eq!(on_mac[0].name, "Pixel 3 XL");
    assert_eq!(
        on_mac[0].kind,
        DeviceKind::Phone,
        "the Mac reports the phone's own kind, from its Config"
    );
    let on_phone = phone.engine.devices();
    assert_eq!(on_phone.len(), 1, "the phone lists the Mac");
    assert_eq!(on_phone[0].name, "Vamana");
    assert_eq!(
        on_phone[0].kind,
        DeviceKind::Mac,
        "the phone reports the Mac's own kind, from its Config"
    );

    // A real file, over a real socket, verified chunk by chunk. The remote
    // path names the phone's one root, "Root".
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let id = mac
        .engine
        .pull(
            on_mac[0].key_hex.clone(),
            "Root/holiday.bin".to_owned(),
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

    let landed = std::fs::read(mac.download_root.join("holiday.bin"))
        .expect("the file should be in the Mac's download folder");
    assert_eq!(landed, bytes, "every byte must match");
    assert!(
        !mac.shared_root.join("holiday.bin").exists(),
        "a pull never writes into a served root"
    );

    let leftovers: Vec<String> = std::fs::read_dir(&mac.download_root)
        .expect("the Mac's download folder should be readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("part"))
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "no partial file should be left behind, found {leftovers:?}"
    );

    // Batch B, item 3, and the C3 fix: the pull dialled the phone, and the
    // phone accepted that connection, so a Wi-Fi success should be on
    // record on both sides, not only the side that dialled. The Mac's own
    // dial already proves this regardless of this machine's own tools.
    // `transport_for_inbound` treats a loopback connection as `Usb` on a
    // machine with no `adb`, the same ambiguity
    // `status_reports_reachability_listen_port_and_adb_presence` below
    // works around, so the phone's side of this only tells the two apart on
    // a machine that has `adb`.
    assert!(
        mac.engine.devices()[0]
            .available_transports
            .contains(&Transport::Wifi),
        "the Mac, which dialled, lists Wifi for the phone"
    );
    if ferry_core::adb::find_adb().is_some() {
        assert!(
            phone.engine.devices()[0]
                .available_transports
                .contains(&Transport::Wifi),
            "the phone, which accepted the connection, lists Wifi for the Mac too"
        );
    }

    // Forgetting takes the device out of the list and takes its transfer
    // records off the disk.
    let phone_key = on_mac[0].key_hex.clone();
    mac.engine
        .forget(phone_key)
        .expect("a paired device can be forgotten");
    assert!(
        mac.engine.devices().is_empty(),
        "the forgotten device must leave the list"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "its transfers must go with it"
    );

    let started = Instant::now();
    mac.engine.stop();
    phone.engine.stop();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "stop took {took:?}");

    // Calling stop twice must be safe.
    mac.engine.stop();
    phone.engine.stop();
}

#[test]
fn the_mac_lists_a_folder_on_the_phone() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");

    phone.engine.set_reachable(true);
    phone.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.start_pairing_with(PairingMethod::Code);

    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);

    let found = mac
        .inbox
        .wait_pairing("the Mac to list a candidate", is_found);
    let PairingState::Found { candidates, .. } = &found else {
        panic!("expected candidates, got {found:?}");
    };
    let wanted = format!("wifi:{phone_addr}");
    let chosen = candidates
        .iter()
        .find(|candidate| candidate.id == wanted)
        .unwrap_or_else(|| panic!("the injected candidate {wanted} should be listed"));
    mac.engine
        .pick_candidate(chosen.id.clone())
        .expect("the candidate should be pickable");

    mac.inbox.wait_pairing("the Mac to show a code", is_code);
    phone
        .inbox
        .wait_pairing("the phone to show a code", is_code);

    mac.engine.confirm_pairing(true);
    phone.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    let phone_key = on_mac[0].key_hex.clone();

    // `Photos` stands in for the first folder a person opens inside the
    // phone's one root, named "Root".
    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("Photos/holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    // The empty path lists every root, not the folder each one holds.
    let top_level = mac
        .engine
        .list(phone_key.clone(), String::new())
        .expect("the roots should list");
    let root_names: Vec<String> = top_level.iter().map(|entry| entry.name.clone()).collect();
    assert_eq!(
        root_names,
        vec!["Root".to_owned()],
        "list(\"\") returns the phone's root names, not what is inside them"
    );

    let entries = mac
        .engine
        .list(phone_key.clone(), "Root/Photos".to_owned())
        .expect("the folder should list");
    let found_entry = entries
        .iter()
        .find(|entry| entry.name == "holiday.bin")
        .expect("the sample file should be in the listing");
    assert_eq!(
        found_entry.size,
        bytes.len() as u64,
        "the listed size must match the file on disk"
    );

    let missing = mac
        .engine
        .list(phone_key, "Root/no-such-folder".to_owned())
        .expect_err("a folder that does not exist cannot be listed");
    assert_eq!(code_of_error(&missing), "OpError::NotFound");

    mac.engine.stop();
    phone.engine.stop();
}
