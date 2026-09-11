//! QR pairing between two engines.
//!
//! `docs/engine-contract.md`, item 12. The phone's camera screen is not
//! built yet, so these test the engine half only: one engine calls
//! `offer_scanned` with another engine's own `Offering` payload, exactly as
//! the contract describes.

mod common;

use common::engines::{
    assert_expires_about_two_minutes_out, build_as, code_of_error, is_confirmed, loopback_addr,
    now_unix_secs, pair, static_key,
};

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use ferry_core::noise::{QR_NONCE_LEN, StaticKey};
use ferry_core::offer::Offer;
use ferry_core::peers::DeviceKind as CoreDeviceKind;
use ferry_core::rpc::exchange_hello;
use ferry_core::tcp;
use ferry_runtime::{DeviceKind, PairingMethod, PairingState, Transport};

/// Replace a real `Offering` payload's addresses with `addr` alone, and
/// re-encode it.
///
/// `local_wifi_addresses` lists this machine's real, non-loopback
/// interfaces, exactly as it does outside a test. On the machine these
/// tests run on, that address is not guaranteed to loop back to this same
/// process the way `loopback_addr` is, so every test here dials the
/// engine's own bound loopback address instead, the same address every
/// other pairing test in this file already dials. Nothing about the format
/// or the handshake is test-only: only which address is in the offer is
/// substituted, the same way a real QR code's bytes would carry whichever
/// address actually works.
fn offer_with_address(payload: &[u8], addr: SocketAddr) -> Vec<u8> {
    let mut offer = Offer::decode(payload).expect("the Mac's own offer should decode");
    offer.addresses = vec![addr];
    offer.encode()
}

fn is_failed(state: &PairingState) -> bool {
    matches!(state, PairingState::Failed { .. })
}

fn is_offering(state: &PairingState) -> bool {
    matches!(state, PairingState::Offering { .. })
}

/// The payload and expiry a `PairingState::Offering` carries, or a panic
/// saying what arrived instead.
fn offer_of(state: &PairingState) -> (Vec<u8>, i64) {
    match state {
        PairingState::Offering { offer } => (offer.payload.clone(), offer.expires_unix_secs),
        other => panic!("expected Offering, got {other:?}"),
    }
}

fn is_idle(state: &PairingState) -> bool {
    matches!(state, PairingState::Idle)
}

fn is_requested(state: &PairingState) -> bool {
    matches!(state, PairingState::Requested { .. })
}

#[test]
fn two_devices_pair_by_scanning_a_qr_code() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    mac.engine.set_reachable(true);
    phone.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let (payload, expires_unix_secs) = offer_of(&offering);
    assert_expires_about_two_minutes_out(expires_unix_secs);
    assert!(
        payload.starts_with(b"FERRY1:"),
        "the drawn payload should be the QR offer format"
    );
    let payload = offer_with_address(&payload, loopback_addr(&mac));

    phone
        .engine
        .offer_scanned(payload)
        .expect("a fresh, unpaired offer should be accepted");

    // The Mac learns the phone's name from the handshake itself, before
    // anyone confirms anything: no second hello is needed for this.
    let requested = mac
        .inbox
        .wait_pairing("the Mac to show the scan", is_requested);
    let PairingState::Requested {
        name,
        kind,
        transport,
    } = requested
    else {
        panic!("expected Requested, got {requested:?}");
    };
    assert_eq!(name, "Pixel 3 XL");
    assert_eq!(kind, DeviceKind::Phone);
    assert_eq!(transport, Transport::Wifi);

    // Both sides confirm. The Mac's confirm releases its hello, and that
    // hello is what names the Mac to the phone, so the phone's own
    // `Requested` can only arrive after it; see `docs/engine-contract.md`
    // item 12.
    mac.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);

    let scanned = phone
        .inbox
        .wait_pairing("the phone to show the Mac it scanned", is_requested);
    let PairingState::Requested {
        name,
        kind,
        transport,
    } = scanned
    else {
        panic!("expected Requested, got {scanned:?}");
    };
    assert_eq!(name, "Vamana");
    assert_eq!(kind, DeviceKind::Mac);
    assert_eq!(transport, Transport::Wifi);

    phone.engine.confirm_pairing(true);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    assert_eq!(on_mac[0].name, "Pixel 3 XL");
    assert_eq!(on_mac[0].kind, DeviceKind::Phone);
    let on_phone = phone.engine.devices();
    assert_eq!(on_phone.len(), 1, "the phone lists the Mac");
    assert_eq!(on_phone[0].name, "Vamana");
    assert_eq!(on_phone[0].kind, DeviceKind::Mac);

    // The phone dialled the offer's own address, so it holds a real address
    // to call the Mac back on; it can list the Mac's shared root.
    let listing = phone
        .engine
        .list(on_phone[0].key_hex.clone(), String::new());
    assert!(
        listing.is_ok(),
        "the phone should list the Mac's root: {listing:?}"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
// J-4: a second hello that disagrees with message one's fails the pairing,
// with the same code an ordinary bad hello gets. Built by hand, driving
// `tcp::pair_ik` and `exchange_hello` directly: no real phone would ever
// claim two different names, so this is the misbehaving peer this crate's
// own dialing code cannot construct.
fn a_second_hello_that_disagrees_with_message_one_fails_pairing() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    mac.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let (payload, _) = offer_of(&offering);
    let addr = loopback_addr(&mac);
    let offer = Offer::decode(&offer_with_address(&payload, addr)).expect("a valid offer");

    let attacker_key = StaticKey::generate().expect("a fresh key pair");
    let dial = std::thread::spawn(move || {
        // Message one's hello: this is what the Mac shows as `Requested`.
        let (mut stream, _socket) = tcp::pair_ik(
            addr,
            &attacker_key,
            &offer.static_key,
            &offer.nonce,
            "Real Name",
            CoreDeviceKind::Phone,
        )
        .expect("the IK handshake should complete with the real nonce");
        // The Mac writes its own hello, then reads this one. A real phone
        // sends the same name twice; this one does not.
        exchange_hello(&mut stream, "Different Name", CoreDeviceKind::Phone)
    });

    let requested = mac
        .inbox
        .wait_pairing("the Mac to show the scan", is_requested);
    let PairingState::Requested { name, .. } = requested else {
        panic!("expected Requested, got {requested:?}");
    };
    assert_eq!(name, "Real Name", "message one's own name is shown first");

    mac.engine.confirm_pairing(true);
    let failed = mac
        .inbox
        .wait_pairing("the disagreeing hello to fail pairing", is_failed);
    let PairingState::Failed { error } = failed else {
        unreachable!("is_failed already matched this");
    };
    assert_eq!(
        code_of_error(&error),
        "RpcError::UnexpectedFrameKind",
        "a disagreeing second hello gets an ordinary bad hello's code"
    );

    // The dial thread's own `exchange_hello` still succeeds from its side:
    // both sides wrote and read one full hello. Only the Mac's comparison
    // rejects the result.
    drop(dial.join().unwrap());

    mac.engine.stop();
}

#[test]
// J-1: a stranger's wrong nonce must not end a live offer.
fn a_strangers_wrong_nonce_does_not_end_a_live_offer() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let stranger = build_as("Someone Else", DeviceKind::Phone);
    mac.engine.set_reachable(true);
    phone.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let (payload, _) = offer_of(&offering);
    let real_payload = offer_with_address(&payload, loopback_addr(&mac));

    // The stranger never saw the real QR code, so it dials with the Mac's
    // real static key and address but a nonce it made up itself.
    let mut rigged = Offer::decode(&real_payload).expect("the Mac's own offer should decode");
    rigged.nonce = [0xAA; QR_NONCE_LEN];
    assert_ne!(
        rigged.nonce,
        Offer::decode(&real_payload).unwrap().nonce,
        "the rigged nonce must actually differ from the real one"
    );
    stranger
        .engine
        .offer_scanned(rigged.encode())
        .expect("the local checks alone do not know the nonce is wrong");
    let stranger_failed = stranger
        .inbox
        .wait_pairing("the stranger's wrong nonce to be refused", is_failed);
    let PairingState::Failed { error } = stranger_failed else {
        unreachable!("is_failed already matched this");
    };
    let code = code_of_error(&error);
    assert!(
        code == "NoiseError::UnknownOffer"
            || code == "NoiseError::Io"
            || code == "VersionError::Io",
        "expected the wrong nonce refused or the connection cut off, got {code}"
    );

    // The Mac's own offer must never have reported a failure: a stranger
    // guessing wrong is not this pairing attempt going wrong.
    assert!(
        !mac.inbox
            .lock()
            .pairings
            .iter()
            .any(|state| matches!(state, PairingState::Failed { .. })),
        "a stranger's wrong nonce must not end a live offer"
    );

    // The real phone can still scan the same, still-live offer and pair
    // normally.
    phone
        .engine
        .offer_scanned(real_payload)
        .expect("a fresh, unpaired offer should still be accepted");
    let requested = mac
        .inbox
        .wait_pairing("the Mac to show the real scan", is_requested);
    let PairingState::Requested { name, .. } = requested else {
        panic!("expected Requested, got {requested:?}");
    };
    assert_eq!(name, "Pixel 3 XL");

    mac.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);
    phone
        .inbox
        .wait_pairing("the phone to show the Mac it scanned", is_requested);
    phone.engine.confirm_pairing(true);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    mac.engine.stop();
    phone.engine.stop();
    stranger.engine.stop();
}

#[test]
fn a_scan_of_an_offer_already_shown_as_requested_is_refused() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let other = build_as("Pixel 6", DeviceKind::Phone);
    // Welcomed even once `Requested`, so this actually reaches the mode
    // dispatch in `handle_inbound` rather than being dropped by the
    // welcome check first; see the guard in `Mode::PairByQr`'s arm.
    mac.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let (payload, _) = offer_of(&offering);
    let payload = offer_with_address(&payload, loopback_addr(&mac));

    phone
        .engine
        .offer_scanned(payload.clone())
        .expect("the first scan should be accepted");
    mac.inbox
        .wait_pairing("the Mac to show the first scan", is_requested);

    // The nonce is single use. This scan reaches the mode dispatch, since
    // the Mac is `reachable`, but `Mode::PairByQr` only calls `pair_ik` while
    // `is_offering()`, which is now false: the connection is dropped, and
    // exactly when that shows up as an error on the scanning side is a race
    // between the version exchange and the `IK` handshake, so either code
    // is accepted; both mean the same thing, the connection was cut off.
    other
        .engine
        .offer_scanned(payload)
        .expect("the local checks alone do not know the code is spent");
    let failed = other
        .inbox
        .wait_pairing("the second scan to be refused", is_failed);
    let PairingState::Failed { error } = failed else {
        unreachable!("is_failed already matched this");
    };
    let code = code_of_error(&error);
    assert!(
        code == "NoiseError::Io" || code == "VersionError::Io",
        "expected an IO cutoff, got {code}"
    );

    mac.engine.stop();
    phone.engine.stop();
    other.engine.stop();
}

#[test]
fn an_expired_offer_is_refused_before_any_dial() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let stranger_key = StaticKey::generate().expect("a fresh key pair");
    let offer = Offer {
        version: 1,
        static_key: stranger_key.public(),
        expires_unix_secs: now_unix_secs() - 1,
        nonce: [1u8; QR_NONCE_LEN],
        // Nothing listens here. If this were ever dialled, the failure
        // would look different (`Runtime::NotReachable`), not
        // `PairingError::OfferExpired`.
        addresses: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1)],
    };

    let error = phone
        .engine
        .offer_scanned(offer.encode())
        .expect_err("an expired offer must be refused");
    assert_eq!(code_of_error(&error), "PairingError::OfferExpired");
    assert!(
        phone.inbox.lock().pairings.is_empty(),
        "no pairing attempt, and so no dial, should ever have started"
    );

    phone.engine.stop();
}

#[test]
fn a_payload_for_an_already_paired_key_is_refused() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let stranger_device = build_as("Some Other Mac", DeviceKind::Mac);
    pair(&stranger_device, &phone);

    let offer = Offer {
        version: 1,
        static_key: static_key(&stranger_device.key).public(),
        expires_unix_secs: now_unix_secs() + 60,
        nonce: [2u8; QR_NONCE_LEN],
        addresses: Vec::new(),
    };
    let error = phone
        .engine
        .offer_scanned(offer.encode())
        .expect_err("a key this device already holds must be refused");
    assert_eq!(code_of_error(&error), "PairingError::AlreadyPaired");

    phone.engine.stop();
    stranger_device.engine.stop();
}

#[test]
fn a_payload_that_is_not_ferry_is_refused() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);

    let error = phone
        .engine
        .offer_scanned(b"not a QR code at all".to_vec())
        .expect_err("a non-Ferry payload must be refused");
    assert_eq!(code_of_error(&error), "PairingError::OfferNotFerry");

    phone.engine.stop();
}

#[test]
// J-6: `offer.rs` refuses `version != 1` with `OfferNotFerry`.
fn an_offer_claiming_version_two_is_refused() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let stranger_key = StaticKey::generate().expect("a fresh key pair");
    let offer = Offer {
        version: 2,
        static_key: stranger_key.public(),
        expires_unix_secs: now_unix_secs() + 60,
        nonce: [3u8; QR_NONCE_LEN],
        addresses: Vec::new(),
    };

    let error = phone
        .engine
        .offer_scanned(offer.encode())
        .expect_err("a version this build does not speak must be refused");
    assert_eq!(code_of_error(&error), "PairingError::OfferNotFerry");
    assert!(
        phone.inbox.lock().pairings.is_empty(),
        "no pairing attempt, and so no dial, should ever have started"
    );

    phone.engine.stop();
}

#[test]
fn cancel_during_offering_returns_to_idle_and_spends_the_offer() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    // As in the refused-second-scan test: welcomed even after cancelling,
    // so the stale scan below reaches the mode dispatch instead of being
    // dropped by the welcome check first.
    mac.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let (payload, _) = offer_of(&offering);
    let payload = offer_with_address(&payload, loopback_addr(&mac));

    mac.engine.cancel_pairing();
    mac.inbox
        .wait_pairing("cancelling to return the Mac to idle", is_idle);

    // The cancelled offer's nonce is gone too, the same as a spent one. As
    // in the refused-second-scan test, either code below means the
    // connection was cut off; which one is a race.
    phone
        .engine
        .offer_scanned(payload)
        .expect("the local checks alone do not know pairing was cancelled");
    let failed = phone
        .inbox
        .wait_pairing("the stale scan to be refused", is_failed);
    let PairingState::Failed { error } = failed else {
        unreachable!("is_failed already matched this");
    };
    let code = code_of_error(&error);
    assert!(
        code == "NoiseError::Io" || code == "VersionError::Io",
        "expected an IO cutoff, got {code}"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
// J-6: `confirm_pairing(false)` on a scanned `Requested` returns to Idle,
// the same as it does on the code method's `Code`.
fn confirm_pairing_false_on_a_requested_scan_returns_to_idle() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    mac.engine.set_reachable(true);
    phone.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let (payload, _) = offer_of(&offering);
    let payload = offer_with_address(&payload, loopback_addr(&mac));

    phone
        .engine
        .offer_scanned(payload)
        .expect("a fresh, unpaired offer should be accepted");
    mac.inbox
        .wait_pairing("the Mac to show the scan", is_requested);

    mac.engine.confirm_pairing(false);
    mac.inbox
        .wait_pairing("refusing to return to Idle", is_idle);
    assert!(
        mac.engine.devices().is_empty(),
        "a refused scan must store no device"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
// J-6: the two minute watchdog turns `Offering` into `Failed` once nobody
// scans in time. A short timeout stands in for the real two minutes.
fn the_watchdog_turns_offering_into_failed() {
    let mac = build_as("Vamana", DeviceKind::Mac);

    mac.engine.set_pairing_timeout(Duration::from_millis(200));
    mac.engine.start_pairing_with(PairingMethod::Qr);
    mac.inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);

    let failed = mac.inbox.wait_pairing("the watchdog to give up", is_failed);
    let PairingState::Failed { error } = failed else {
        unreachable!("is_failed already matched this");
    };
    assert_eq!(code_of_error(&error), "Runtime::PairingTimeout");

    mac.engine.stop();
}
