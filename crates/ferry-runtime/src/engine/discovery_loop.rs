//! Browsing mDNS and turning what it finds into pairing candidates.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ferry_core::discovery::{Browser, Event};

use crate::notify::Change;
use crate::state::{Candidate, lock};
use crate::{PairingCandidate, Transport};

use super::{BROWSE_TICK, MAX_CANDIDATES, MAX_DISCOVERED, Shared, notify};

/// Watch mDNS for the whole session, while browsing is allowed.
///
/// `docs/engine-contract.md`, item 18. The thread runs for the whole session
/// as it always did, but the `Browser` only exists while `Shared::browsing`
/// is true, because a browse query is a sound on the network and a device in
/// a café makes none. This does not need `reachable`: it is what lets a Mac
/// with its presence switch off still find and mount a phone. A `Browser`
/// this loop drops shuts its own mDNS daemon down, so nothing is left
/// querying.
pub(crate) fn browse_loop(shared: &Arc<Shared>) {
    let mut browser: Option<Browser> = None;
    while !shared.stopping() {
        if !shared.browsing.load(Ordering::SeqCst) {
            browser = None;
            if !shared.rest(BROWSE_TICK) {
                break;
            }
            continue;
        }
        if browser.is_none() {
            let Ok(started) = Browser::start() else {
                // A network that refuses multicast leaves the cable and a
                // known address, both of which work without this loop. Trying
                // again on the next pass costs one daemon start per
                // `BROWSE_TICK`, which is what a healthy loop spends on one
                // `next` call anyway, and it is what lets a browser appear
                // when browsing becomes allowed again on a network that does
                // allow multicast.
                if !shared.rest(BROWSE_TICK) {
                    break;
                }
                continue;
            };
            browser = Some(started);
        }
        if let Some(found) = browser.as_ref() {
            match found.next(BROWSE_TICK) {
                Some(Event::Found {
                    instance,
                    addr,
                    version: _,
                }) => on_discovered(shared, &instance, addr),
                Some(Event::Lost { instance }) => {
                    lock(&shared.state)
                        .pairing
                        .candidates
                        .remove(&format!("wifi:{instance}"));
                }
                None => {}
            }
        }
    }
}

/// Keep an address worth dialing later, newest first.
pub(crate) fn remember_address(shared: &Arc<Shared>, addr: SocketAddr) {
    let mut state = lock(&shared.state);
    state.discovered.retain(|known| *known != addr);
    state.discovered.insert(0, addr);
    state.discovered.truncate(MAX_DISCOVERED);
}

/// Record an address discovery found, and offer it while pairing.
fn on_discovered(shared: &Arc<Shared>, instance: &str, addr: SocketAddr) {
    remember_address(shared, addr);
    let shown = PairingCandidate {
        id: format!("wifi:{instance}"),
        transport: Transport::Wifi,
        short_code: last_four(instance),
    };
    add_candidate(shared, &shown, addr);
}

/// The candidate an injected address becomes.
pub(crate) fn wifi_candidate(addr: SocketAddr) -> PairingCandidate {
    let text = addr.to_string();
    PairingCandidate {
        id: format!("wifi:{text}"),
        transport: Transport::Wifi,
        short_code: last_four(&text),
    }
}

/// The last four characters, which is what both screens show.
pub(crate) fn last_four(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

/// Put one candidate in front of the person, if pairing is still open.
///
/// The list is capped, and the change is reported at most once a second. The
/// whole list crosses the boundary with every report, so a network that
/// answers a thousand times must not become a thousand reports of a thousand
/// candidates each.
pub(crate) fn add_candidate(shared: &Arc<Shared>, shown: &PairingCandidate, addr: SocketAddr) {
    {
        let mut state = lock(&shared.state);
        if !state.pairing.is_open_to_pairing() {
            return;
        }
        let known = state.pairing.candidates.contains_key(&shown.id);
        if !known && state.pairing.candidates.len() >= MAX_CANDIDATES {
            return;
        }
        state.pairing.candidates.insert(
            shown.id.clone(),
            Candidate {
                shown: shown.clone(),
                addr,
            },
        );
    }
    notify(shared, Change::Found);
}
