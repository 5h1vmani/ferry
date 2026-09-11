//! Accepting a connection and deciding who is calling.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ferry_core::limits::MAX_INBOUND_CONNECTIONS;
use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::tcp::{Listener, Pending};
use ferry_core::version::Mode;

use crate::Transport;
use crate::networks;
use crate::state::lock;

use super::{
    Shared, accept_pairing, accept_qr_offer, candidate_peers, serve_connection, welcomes_inbound,
};

/// Reserves one of [`MAX_INBOUND_CONNECTIONS`] slots, or `None` when the
/// engine already holds that many. Mirrors `ConnectionSlot` in
/// `dav/server.rs`: a slot can only be created while one is free, and
/// dropping it is the only way to free one again, so the count can never
/// be missed or double counted. `docs/audits/fable-engineering.md`,
/// finding 2.
pub(crate) struct InboundSlot(Arc<Shared>);

impl Drop for InboundSlot {
    fn drop(&mut self) {
        self.0.inbound.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn reserve_inbound_slot(shared: &Arc<Shared>) -> Option<InboundSlot> {
    shared
        .inbound
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_INBOUND_CONNECTIONS).then_some(current + 1)
        })
        .ok()?;
    Some(InboundSlot(Arc::clone(shared)))
}

/// Accept connections until the engine stops.
pub(crate) fn accept_loop(shared: &Arc<Shared>, net: &Arc<Listener>) {
    while !shared.stopping() {
        let Ok(pending) = net.accept() else {
            // Too many handshakes at once, or a socket error. Either way the
            // next connection is the one that matters.
            continue;
        };
        if shared.stopping() {
            drop(pending);
            continue;
        }
        let remote = pending.remote();
        let welcome = {
            let state = lock(&shared.state);
            welcomes_inbound(
                state.reachable,
                networks::wifi_presence(&state),
                state.pairing.accepts_inbound(),
                remote,
            )
        };
        if !welcome {
            // This is what `set_reachable(false)`, and an untrusted network,
            // mean. The connection is dropped after accept, not before,
            // because a listener cannot refuse before accepting.
            drop(pending);
            continue;
        }
        // `docs/audits/fable-engineering.md`, finding 2: a peer on a
        // trusted network could otherwise open connections in a loop
        // until thread creation itself failed. Refused here, before a
        // thread is even asked for, the same way a pending handshake past
        // its own cap is refused in `ferry_core::tcp`.
        let Some(slot) = reserve_inbound_slot(shared) else {
            drop(pending);
            continue;
        };
        shared.accepted.fetch_add(1, Ordering::SeqCst);
        let shared = Arc::clone(shared);
        // A serving thread is not joined. See the crate documentation.
        //
        // `Builder::spawn` rather than the panicking `thread::spawn`: when
        // the OS refuses a new thread, `pending` (moved into the closure
        // below) is simply dropped along with it, closing the socket, and
        // the accept loop keeps running instead of taking the whole
        // engine down with it. `docs/audits/fable-engineering.md`,
        // finding 2.
        let spawned = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            handle_inbound(&shared, pending);
        });
        drop(spawned);
    }
}

/// Decide what one accepted connection is, and run it.
///
/// `docs/engine-contract.md` item 12: the pre-handshake exchange now carries
/// a mode byte, so this reads it once, with `Pending::negotiate`, before it
/// picks a Noise pattern, rather than guessing purely from local state.
/// Local state still gates each mode: a `PairByCode` or `PairByQr` request
/// is only honoured while this device is actually open to that one method,
/// and a mismatched request is simply dropped, the same way a `Connect`
/// request from a stranger with no matching key already was.
pub(crate) fn handle_inbound(shared: &Arc<Shared>, pending: Pending) {
    let remote = pending.remote();
    let Ok(negotiated) = pending.negotiate() else {
        // A version or a handshake timeout. Nothing to report: a peer that
        // cannot even negotiate learns nothing more by being told so.
        return;
    };
    match negotiated.mode() {
        Mode::PairByCode => {
            if lock(&shared.state).pairing.is_open_to_pairing() {
                accept_pairing(shared, negotiated, remote);
            }
        }
        Mode::PairByQr => {
            if lock(&shared.state).pairing.is_offering() {
                accept_qr_offer(shared, negotiated, remote);
            }
        }
        Mode::Connect => {
            // With no stored peer there is nobody this connection could be, and
            // `candidate_peers` still returns one candidate nobody holds, so a
            // device with no peer and a device with one peer look the same from
            // outside. Dropping the connection here instead would tell a stranger
            // which of the two this device is.
            let candidates = candidate_peers(shared, remote);
            // A refused handshake is the design working: whoever called does not
            // hold a key this device paired with. Nothing to report.
            if let Ok(connection) = negotiated.connect(&shared.key, &candidates) {
                let peer = connection.peer;
                serve_connection(shared, connection, peer);
            }
        }
    }
}

/// A key no caller can hold the other half of.
///
/// A fresh key pair is thrown away as soon as its public half is taken, so
/// the handshake that follows cannot succeed. If the system gives no random
/// bytes, this device's own public key is used instead, which no caller
/// holds the private half of either.
pub(crate) fn nobody(shared: &Arc<Shared>) -> PublicKey {
    StaticKey::generate().map_or_else(|_| shared.key.public(), |key| key.public())
}

/// A connection arriving on the loopback address came through an `adb`
/// forward, unless this machine is the one that made the forward.
///
/// Only the Mac runs `adb`. So a loopback connection into a device with no
/// `adb` is the cable, and a loopback connection into a machine that has
/// `adb` is another program on the same machine.
pub(crate) fn transport_for_inbound(shared: &Arc<Shared>, remote: SocketAddr) -> Transport {
    if remote.ip().is_loopback() && shared.adb.is_none() {
        Transport::Usb
    } else {
        Transport::Wifi
    }
}
