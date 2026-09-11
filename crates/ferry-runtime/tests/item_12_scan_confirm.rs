//! Item 12: the scanning side asks before it stores.
//!
//! The phone scans the Mac's QR code, dials it, and runs the `IK`
//! handshake. That proves the key came from the screen. It does not show
//! the person a name. So the scanning side now publishes
//! `PairingState::Requested` with the name the hello carried, and stores
//! the peer only when `confirm_pairing(true)` arrives. A scan of a
//! stranger's code can then be refused by name, before anything is stored
//! and before the network is trusted.
//!
//! The offering Mac is unchanged. It still shows `Requested` with the
//! phone's name and confirms on its own side.
//!
//! The harness here is copied from the QR section of `two_engines.rs`, per
//! `docs/agent-runs.md` rule 3: one test file per item.

mod common;

use common::engines::{Side, build_as, is_confirmed, loopback_addr};

use std::net::SocketAddr;

use ferry_core::offer::Offer;
use ferry_runtime::{DeviceKind, PairingMethod, PairingState, Transport};

fn is_offering(state: &PairingState) -> bool {
    matches!(state, PairingState::Offering { .. })
}

fn is_requested(state: &PairingState) -> bool {
    matches!(state, PairingState::Requested { .. })
}

fn is_idle(state: &PairingState) -> bool {
    matches!(state, PairingState::Idle)
}

/// True for either state a scan can settle into on the scanning side.
///
/// Waiting on this rather than on `Requested` alone is what keeps a failing
/// run short. An engine that stores without asking reports `Confirmed`,
/// this returns at once, and the caller panics with the state it actually
/// got instead of waiting out `PATIENCE`.
fn is_requested_or_confirmed(state: &PairingState) -> bool {
    is_requested(state) || is_confirmed(state)
}

/// The payload a `PairingState::Offering` carries, or a panic saying what
/// arrived instead.
fn payload_of(state: &PairingState) -> Vec<u8> {
    match state {
        PairingState::Offering { offer } => offer.payload.clone(),
        other => panic!("expected Offering, got {other:?}"),
    }
}

/// Replace a real `Offering` payload's addresses with `addr` alone, and
/// re-encode it.
///
/// The same substitution the QR tests in `two_engines.rs` make, and for the
/// same reason. These engines dial each other on the loopback address they
/// actually bound, not on this machine's real interfaces.
fn offer_with_address(payload: &[u8], addr: SocketAddr) -> Vec<u8> {
    let mut offer = Offer::decode(payload).expect("the Mac's own offer should decode");
    offer.addresses = vec![addr];
    offer.encode()
}

/// Drive both engines as far as the scanning side's own question.
///
/// Returns the two sides, with the Mac already `Confirmed` and the phone
/// holding whatever it settled on. The Mac's confirm is what releases its
/// hello, and the hello is what carries its name, so the phone cannot be
/// asked anything before the Mac has answered its own question.
fn scan_up_to_the_phones_question() -> (Side, Side) {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    mac.engine.set_reachable(true);
    phone.engine.set_reachable(true);

    mac.engine.start_pairing_with(PairingMethod::Qr);
    let offering = mac
        .inbox
        .wait_pairing("the Mac to offer a QR code", is_offering);
    let payload = offer_with_address(&payload_of(&offering), loopback_addr(&mac));

    phone
        .engine
        .offer_scanned(payload)
        .expect("a fresh, unpaired offer should be accepted");
    mac.inbox
        .wait_pairing("the Mac to show the scan", is_requested);

    mac.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);
    (mac, phone)
}

#[test]
// The scanning side names the Mac and stores nothing until it is answered.
fn a_scan_asks_before_it_stores() {
    let (mac, phone) = scan_up_to_the_phones_question();

    let settled = phone.inbox.wait_pairing(
        "the phone to settle after the scan",
        is_requested_or_confirmed,
    );
    let PairingState::Requested {
        name,
        kind,
        transport,
    } = settled
    else {
        panic!("the scanning side must ask before it stores, got {settled:?}");
    };
    assert_eq!(name, "Vamana", "the phone shows the Mac's own name");
    assert_eq!(kind, DeviceKind::Mac);
    assert_eq!(transport, Transport::Wifi);
    assert!(
        phone.engine.devices().is_empty(),
        "nothing is stored on the scanning side before its own confirm"
    );

    // `cancel_pairing` drops the question the same way a refusal does, and
    // it releases the held session with it. Ending the pairing here also
    // keeps this test quick: the Mac serves on that session after its own
    // confirm, and `Engine::stop` on the Mac waits for that serving thread.
    phone.engine.cancel_pairing();
    phone
        .inbox
        .wait_pairing("cancelling to return the phone to Idle", is_idle);
    assert!(
        phone.engine.devices().is_empty(),
        "a cancelled scan stores nothing either"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
// Both sides confirm, and both sides then hold the other device.
fn both_sides_store_after_their_own_confirm() {
    let (mac, phone) = scan_up_to_the_phones_question();

    let settled = phone.inbox.wait_pairing(
        "the phone to settle after the scan",
        is_requested_or_confirmed,
    );
    assert!(
        is_requested(&settled),
        "the scanning side must ask before it stores, got {settled:?}"
    );

    phone.engine.confirm_pairing(true);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    let on_phone = phone.engine.devices();
    assert_eq!(on_phone.len(), 1, "the phone lists the Mac");
    assert_eq!(on_phone[0].name, "Vamana");
    assert_eq!(on_phone[0].kind, DeviceKind::Mac);

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    assert_eq!(on_mac[0].name, "Pixel 3 XL");
    assert_eq!(on_mac[0].kind, DeviceKind::Phone);

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
// A refused scan stores nothing on the side that refused it.
fn a_refused_scan_stores_nothing_on_the_scanning_side() {
    let (mac, phone) = scan_up_to_the_phones_question();

    let settled = phone.inbox.wait_pairing(
        "the phone to settle after the scan",
        is_requested_or_confirmed,
    );
    assert!(
        is_requested(&settled),
        "the scanning side must ask before it stores, got {settled:?}"
    );

    phone.engine.confirm_pairing(false);
    phone
        .inbox
        .wait_pairing("refusing to return the phone to Idle", is_idle);
    assert!(
        phone.engine.devices().is_empty(),
        "a refused scan must store no device on the side that refused it"
    );

    // The offering side already answered its own question, so it already
    // holds the phone. Each side stores after its own confirm, and one
    // side's refusal never reaches into the other side's store. The person
    // on the Mac drops that device in Devices, the same as any other.
    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac kept what its own confirm stored");
    assert_eq!(on_mac[0].name, "Pixel 3 XL");

    mac.engine.stop();
    phone.engine.stop();
}
