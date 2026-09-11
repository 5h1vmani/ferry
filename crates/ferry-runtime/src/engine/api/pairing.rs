//! The five methods the apps call to run pairing, both ways round. The
//! handshakes themselves are `engine::pairing`; this is only the surface
//! an app calls to start one, answer a candidate, or confirm what it shows.

use std::sync::Arc;

use ferry_core::offer::{Offer, PairingError as OfferError};

use crate::engine::{
    Confirming, Engine, begin_pairing_deadline, dial_for_pairing, dial_offer, pair_after_confirm,
    start_offering,
};
use crate::errors::{failed, from_offer};
use crate::state::{lock, now_unix_secs};
use crate::{FerryError, PairingMethod, PairingState};

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
    /// Enter pairing, by `method`. Replaces the old `start_pairing`. Times
    /// out after two minutes either way.
    ///
    /// `Code`: the Mac browses and polls `adb`, and reports candidates. The
    /// phone waits for one `XX` handshake and reports the code.
    ///
    /// `Qr`: makes a nonce and an offer for this device's Wi-Fi addresses,
    /// and publishes `Offering`. While offering, one `IK` handshake whose
    /// message one carries the current nonce is accepted; it shows
    /// `Requested` and holds the connection for `confirm_pairing`. Meant for
    /// the Mac; the phone's camera screen is not built yet, so nothing
    /// today calls this with `Qr` on a phone.
    ///
    /// Calling this while a pairing is already running only reports the
    /// current state again, under either method.
    pub fn start_pairing_with(&self, method: PairingMethod) {
        // The running check and the claim below happen under one lock, in
        // `begin_pairing_deadline`: a second call cannot land in the gap
        // between them the way two separate lock acquisitions would allow.
        let Some(expires_unix_secs) = begin_pairing_deadline(&self.shared) else {
            let shown = lock(&self.shared.state).pairing.shown.clone();
            self.shared.notify.pairing(&shown);
            return;
        };
        match method {
            PairingMethod::Code => {
                self.shared
                    .set_pairing(&PairingState::Waiting { expires_unix_secs });
            }
            PairingMethod::Qr => start_offering(&self.shared, expires_unix_secs),
        }
    }

    /// Phone only. The bytes its camera decoded from the Mac's QR code.
    ///
    /// Checked locally, in order: is this a Ferry offer at all, has it
    /// expired, is its key one this device already holds. Any of those
    /// three refuses at once, before a single byte reaches the network.
    /// Past that point the dial and the `IK` handshake run on their own
    /// thread, as `pick_candidate` runs its dial, and the outcome arrives
    /// through the listener. Once the names cross, this device publishes
    /// `Requested` with the other device's name and waits for
    /// `confirm_pairing`, the same as the offering Mac does. The scan proves
    /// the key came from a screen; it does not show whose screen, so this
    /// side asks that question before it stores anything.
    ///
    /// # Errors
    ///
    /// Returns `PairingError::OfferNotFerry`, `PairingError::OfferExpired`,
    /// or `PairingError::AlreadyPaired` for the three local checks above,
    /// and `Runtime::PairingBusy` when a pairing attempt is already running
    /// on this device.
    pub fn offer_scanned(&self, payload: Vec<u8>) -> Result<(), FerryError> {
        let offer = Offer::decode(&payload).map_err(from_offer)?;
        if offer.is_expired(now_unix_secs()) {
            return Err(from_offer(OfferError::OfferExpired));
        }
        if lock(&self.shared.state)
            .peers
            .get(&offer.static_key)
            .is_some()
        {
            return Err(from_offer(OfferError::AlreadyPaired));
        }
        // The running check and the claim happen under one lock; see
        // `begin_pairing_deadline`.
        let Some(expires_unix_secs) = begin_pairing_deadline(&self.shared) else {
            return Err(failed("Runtime::PairingBusy"));
        };
        self.shared
            .set_pairing(&PairingState::Waiting { expires_unix_secs });

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || dial_offer(&shared, &offer)));
        Ok(())
    }

    /// Dial the chosen candidate and run the pairing handshake.
    ///
    /// The dial happens on its own thread, so this returns at once. The code
    /// arrives through the listener.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NoCandidate` when that candidate is not listed, and
    /// `Runtime::PairingBusy` when a code is already showing or another
    /// candidate is already being dialed.
    pub fn pick_candidate(&self, id: String) -> Result<(), FerryError> {
        let addr = {
            let mut state = lock(&self.shared.state);
            if !state.pairing.is_running() {
                return Err(failed("Runtime::NoCandidate"));
            }
            if !state.pairing.is_open_to_pairing() {
                return Err(failed("Runtime::PairingBusy"));
            }
            let addr = state
                .pairing
                .candidates
                .get(&id)
                .map(|c| c.addr)
                .ok_or_else(|| failed("Runtime::NoCandidate"))?;
            // One dial at a time, from this moment and not from the moment
            // the handshake finishes. Two dials would show two codes.
            state.pairing.dialing = true;
            addr
        };

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || dial_for_pairing(&shared, addr)));
        Ok(())
    }

    /// Accept or reject the device whose code, or scan, is showing.
    ///
    /// Accepting stores the peer, and on most paths exchanges names first.
    /// That takes a round trip, so it runs on its own thread and reports
    /// through the listener. Works the same way for both pairing methods
    /// and for both sides of a scan: whichever of `held` (code) or
    /// `requested` (QR) is holding a connection is the one taken.
    ///
    /// This answers for this device only. The other device answers its own
    /// question on its own screen, and neither answer stores anything on
    /// the other. `docs/engine-contract.md` item 12.
    pub fn confirm_pairing(&self, accept: bool) {
        let taken = {
            let mut state = lock(&self.shared.state);
            state
                .pairing
                .held
                .take()
                .map(Confirming::Code)
                .or_else(|| state.pairing.requested.take().map(Confirming::Scan))
        };
        let Some(taken) = taken else {
            if !accept {
                self.shared.set_pairing(&PairingState::Idle);
            }
            return;
        };
        if !accept {
            drop(taken);
            self.shared.set_pairing(&PairingState::Idle);
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.shared.keep(std::thread::spawn(move || {
            pair_after_confirm(&shared, taken);
        }));
    }

    /// Stop pairing and drop whatever it was holding, under either method.
    pub fn cancel_pairing(&self) {
        {
            let mut state = lock(&self.shared.state);
            state.pairing.held = None;
            state.pairing.dialing = false;
            state.pairing.requested = None;
            state.pairing.offer_nonce = None;
        }
        self.shared.set_pairing(&PairingState::Idle);
    }
}
