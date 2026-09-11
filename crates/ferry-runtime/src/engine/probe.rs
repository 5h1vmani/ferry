//! One short dial that proves a paired device is reachable.
//!
//! `docs/engine-contract.md`, item 3. Every other caller of
//! [`super::mark_reachable`] is a dial this device makes for work of its
//! own, or a connection it accepts and serves. So a device only ever became
//! reachable once the other side happened to connect. The phone found the
//! Mac on mDNS, wrote the address down, and still showed it as not
//! reachable, so the first open of the phone's Files app said "not
//! reachable" until the Mac dialled for some other reason.
//!
//! A probe closes that. When discovery gives this engine an address it did
//! not already have at the front of its list, every paired device gets one
//! short dial: [`crate::pool::Pool::take_dialing`], which dials, calls
//! `mark_reachable` on success, and exchanges the hello. The borrow is
//! dropped at once, so the connection goes back to the pool idle and the
//! next real call reuses it rather than dialling again. That is the whole
//! probe: one hello and one pooled connection, no listing and no transfer.
//!
//! Discovery cannot say which paired device an address belongs to, because
//! the mDNS instance name is random and a found address is anonymous until
//! a handshake succeeds (item 3). So the probe asks every paired device,
//! and `transfer::dial` tries that device's own addresses in its usual
//! order. A device that is not there fails its dial and nothing is marked.
//!
//! Two bounds stop a flapping advert becoming a dial storm. An address that
//! is already the newest one this engine knows starts no probe at all. And
//! one device is probed at most once every [`PROBE_MIN_INTERVAL_SECS`]
//! seconds, whatever discovery does in between.
//!
//! The engine is symmetric, so this runs on both sides: the Mac probes the
//! phone the same way the phone probes the Mac.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::notify::Change;
use crate::state::{hex_of, lock};

use super::{PROBE_MIN_INTERVAL_SECS, Shared, notify};

/// Probe every paired device once, because discovery just gave this engine
/// an address it did not already hold as the newest one.
pub(crate) fn on_address_found(shared: &Arc<Shared>) {
    if shared.stopping() {
        return;
    }
    // The state lock is released before any probe starts, because starting
    // one takes the probe clock and then spawns a thread.
    let devices: Vec<String> = lock(&shared.state)
        .peers
        .all()
        .iter()
        .map(|peer| hex_of(&peer.key))
        .collect();
    for device_key_hex in devices {
        start_one(shared, &device_key_hex);
    }
}

/// Start one probe for `device_key_hex`, unless one started for it within
/// the last [`PROBE_MIN_INTERVAL_SECS`] seconds.
///
/// The clock is taken and the count raised before the thread is spawned, so
/// a caller that has just returned from a discovery event can see exactly
/// how many probes that event started.
fn start_one(shared: &Arc<Shared>, device_key_hex: &str) {
    {
        let now = Instant::now();
        let mut last = lock(&shared.last_probe);
        if let Some(started) = last.get(device_key_hex)
            && now.duration_since(*started) < Duration::from_secs(PROBE_MIN_INTERVAL_SECS)
        {
            return;
        }
        last.insert(device_key_hex.to_owned(), now);
    }
    shared.probes.fetch_add(1, Ordering::SeqCst);
    // Stopping may have begun in the moment since the check above. Checked
    // again here, right before spawning, so a shutdown racing this call is
    // not handed a fresh dial to a peer it is about to stop talking to.
    // `keep` closes whatever gap is left, the same way `auto_copy` does.
    if shared.stopping() {
        return;
    }
    let shared_for_thread = Arc::clone(shared);
    let key = device_key_hex.to_owned();
    shared.keep(std::thread::spawn(move || run(&shared_for_thread, &key)));
}

/// One probe: borrow a pooled connection, then give it straight back.
///
/// A dial against a stale address can still take as long as the operating
/// system's own connect timeout, because `ferry_core::tcp` carries no
/// connect timeout of its own. That is the limitation `pool.rs` already
/// documents for every caller of `transfer::dial`, and this probe shares
/// it; it is not fixed here.
fn run(shared: &Arc<Shared>, device_key_hex: &str) {
    let Ok(pool) = shared.pool_for(device_key_hex) else {
        return;
    };
    // `take_dialing` calls `mark_reachable` itself once the dial succeeds.
    // Dropping the borrow at the end of this statement gives the connection
    // back to the pool idle.
    if pool.take_dialing(shared).is_ok() {
        // `mark_reachable` writes the fact but tells nobody. The app has to
        // hear it, or the device list still shows "not reachable" until the
        // next change of something else.
        notify(shared, Change::Devices);
    }
}
