//! Item 16, part c: a pairing socket must be visible to `stop`.
//!
//! `hold_pairing`, `hold_qr_pairing`, `dial_offer`, and `dial_for_pairing`
//! each hold a socket while a person compares codes or a scan waits to be
//! confirmed, which can run for the whole two minute pairing deadline.
//! Before this fix, none of those sockets were registered with `Shared`,
//! so `stop`'s "close every socket" loop had nothing to shut down for any
//! of them.
//!
//! That does not make `Engine::stop` itself slow: the pairing watchdog
//! thread it joins wakes on `Shared`'s own condition variable regardless,
//! so a plain "hold a code, then stop" measurement returns in well under a
//! second whether or not the socket is registered, and proves nothing.
//! What the missing registration actually breaks is the *other* side of
//! the held connection: with nobody telling the OS to close it, that
//! side's own copy of the socket stays open, so it does not learn the
//! pairing is dead until it gives up on its own. This test confirms on
//! the peer, by having it confirm a pairing whose other end already
//! stopped, and timing how long it takes to notice.
//!
//! The harness is the shared one in `tests/common/engines.rs`. The
//! code-pairing tests it was written beside are now
//! `tests/two_engines_pairing.rs`.

mod common;

use common::engines::{build_as, is_code, is_found, loopback_addr};

use std::time::{Duration, Instant};

use ferry_runtime::{DeviceKind, PairingMethod, PairingState};

fn is_failed(state: &PairingState) -> bool {
    matches!(state, PairingState::Failed { .. })
}

/// `docs/engine-contract.md` item 16c: every socket a pairing path holds
/// must be registered with `Shared`, the same as an ordinary connection,
/// so `stop` shuts it down directly instead of leaving it open.
///
/// This drives the code-pairing dial by hand, the same way `pair`
/// does, and stops the Mac the moment both sides show the same code: the
/// held state a person would otherwise sit in while comparing digits.
///
/// `mac.engine.stop()` itself already returns quickly either way, because
/// the pairing watchdog thread it joins wakes on `Shared`'s own condition
/// variable regardless of whether the held socket is registered. The
/// budget is asserted anyway, since it must hold, but it is not what this
/// test turns on. What actually depends on the fix is the phone: it still
/// holds its own end of the same connection, and confirms right after the
/// Mac stops. `finish_pairing`'s name exchange
/// (`crates/ferry-runtime/src/engine.rs`, `hello_with_deadline`) gives up
/// on its own after `FINISH_PAIRING_DEADLINE`, ten seconds, so without the
/// fix the phone learns nothing is coming until that deadline passes.
/// With the fix, the Mac's `stop` calls `shutdown` on its clone of the
/// socket, the phone's read fails at once, and it reports `Failed` in a
/// small fraction of that budget.
#[test]
fn a_confirming_peer_learns_a_stopped_pairing_died_at_once() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);

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

    // Both sides now hold a connection and show the same code, waiting for
    // a person to confirm. This is the held state a `stop` mid-pairing
    // must cut short.
    mac.inbox.wait_pairing("the Mac to show a code", is_code);
    phone
        .inbox
        .wait_pairing("the phone to show a code", is_code);

    let started = Instant::now();
    mac.engine.stop();
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(5),
        "stop took {took:?} while a code pairing was held"
    );

    // The phone still holds its half of the connection to a Mac that just
    // stopped. Confirming drives `finish_pairing`'s name exchange over it.
    let confirm_started = Instant::now();
    phone.engine.confirm_pairing(true);
    phone
        .inbox
        .wait_pairing("the phone to learn the pairing died", is_failed);
    let confirm_took = confirm_started.elapsed();
    assert!(
        confirm_took < Duration::from_secs(3),
        "the phone took {confirm_took:?} to learn its pairing died; \
         the Mac's stop should close the socket it was holding at once, \
         instead of leaving the phone to wait out its own ten second \
         name-exchange deadline"
    );

    phone.engine.stop();
}
