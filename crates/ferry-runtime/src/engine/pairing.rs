//! Pairing, both ways round.
//!
//! The code method dials a discovered candidate and runs XX. The QR
//! method makes an offer the other side scans and runs IK. Both end in
//! a stored peer or a reported failure, and both time out.

use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use ferry_core::noise::{NoiseError, PublicKey, QR_NONCE_LEN, SecureStream};
use ferry_core::offer::Offer;
use ferry_core::peers::{DeviceKind as CoreDeviceKind, Peer};
use ferry_core::rpc::exchange_hello;
use ferry_core::tcp::{self, IkPairedConnection, NegotiatedPending, PairedConnection, TcpError};

use crate::errors::{failed, from_rpc, from_tcp};
use crate::guard::StopAware;
use crate::notify::Change;
use crate::state::{HeldPairing, Pairing, RequestedPairing, hex_of, lock, now_unix_secs};
use crate::{FerryError, PairingOffer, PairingState, Transport};

use super::{
    FINISH_PAIRING_DEADLINE, FINISH_PAIRING_TICK, Shared, SocketRegistration, nobody, notify,
    register_serving, save_networks, save_peers, serve_named_stream, transport_for_inbound,
};

/// Run the code pairing handshake as the side that accepted the connection.
pub(crate) fn accept_pairing(
    shared: &Arc<Shared>,
    negotiated: NegotiatedPending,
    remote: SocketAddr,
) {
    match negotiated.pair(&shared.key) {
        // `docs/audits/fable-security.md`, finding 4: a refused hold used to
        // report nothing, so a real device arriving while a stranger's
        // connection already held the code slot saw no failure at all, only
        // a code that was never theirs. `fail_pairing` reports it now, and
        // does nothing when pairing already moved on for its own reason.
        Ok(connection) => {
            if let Err(error) = hold_pairing(shared, connection, remote, true) {
                fail_pairing(shared, error);
            }
        }
        Err(error) => report_pairing_failure(shared, &error),
    }
}

/// Run the QR pairing handshake as the side whose key was in the offer.
pub(crate) fn accept_qr_offer(
    shared: &Arc<Shared>,
    negotiated: NegotiatedPending,
    remote: SocketAddr,
) {
    let expected_nonce = lock(&shared.state).pairing.offer_nonce;
    match negotiated.pair_ik(&shared.key, expected_nonce.as_ref()) {
        // A refused hold means another scan is already `Requested`. Nothing
        // to report; see `hold_qr_pairing`.
        Ok(connection) => drop(hold_qr_pairing(shared, connection, remote)),
        // A wrong or guessed nonce is a stranger, not a failure of this
        // offer: the connection is simply dropped, and the offer stays
        // live for the real phone to still scan and complete. Every other
        // handshake failure still ends the offer, the same as any other
        // pairing failure does.
        Err(TcpError::Noise(NoiseError::UnknownOffer)) => {}
        Err(error) => report_pairing_failure(shared, &error),
    }
}

/// Report a failed handshake, unless pairing has already moved on.
///
/// Someone who cancelled while the handshake ran must not see a failure for
/// a pairing they already stopped.
fn report_pairing_failure(shared: &Arc<Shared>, error: &TcpError) {
    if !lock(&shared.state).pairing.is_running() {
        return;
    }
    shared.set_pairing(&PairingState::Failed {
        error: from_tcp(error),
    });
}

/// Dial one candidate and run the pairing handshake as the initiator.
pub(crate) fn dial_for_pairing(shared: &Arc<Shared>, addr: SocketAddr) {
    match tcp::pair(addr, &shared.key) {
        Ok(connection) => {
            // A refused hold usually means the pairing moved on for its own
            // reason while this dial ran, in which case `fail_pairing`'s own
            // `is_running` check makes it a no-op. `docs/audits/fable-security.md`,
            // finding 4: it can also mean a stranger's connection already
            // holds the code slot, and that failure is now reported instead
            // of silently dropping the connection.
            if let Err(error) = hold_pairing(shared, connection, addr, false) {
                fail_pairing(shared, error);
            }
        }
        Err(error) => {
            lock(&shared.state).pairing.dialing = false;
            report_pairing_failure(shared, &error);
        }
    }
}

/// Show the code and hold the connection until someone confirms.
///
/// # Errors
///
/// Returns `Runtime::PairingBusy` when something is already held, or when
/// pairing has moved on. Callers report this with `fail_pairing`, so a
/// second code never replaces the one the person is comparing, but the
/// person still sees that the connection which just arrived did not get
/// one. `docs/audits/fable-security.md`, finding 4.
fn hold_pairing(
    shared: &Arc<Shared>,
    connection: PairedConnection,
    addr: SocketAddr,
    accepted: bool,
) -> Result<(), FerryError> {
    let PairedConnection {
        paired,
        version: _,
        socket: raw_socket,
    } = connection;
    let code = paired.code.to_string();
    let expires_unix_secs;
    {
        let mut state = lock(&shared.state);
        if !accepted {
            // This dial is finished, whether or not its code is shown.
            state.pairing.dialing = false;
        }
        if !state.pairing.is_open_to_pairing() {
            return Err(failed("Runtime::PairingBusy"));
        }
        expires_unix_secs = state
            .pairing
            .deadline_unix_secs
            .unwrap_or_else(now_unix_secs);
        // `docs/engine-contract.md` item 16c: a code pairing is held for as
        // long as the pairing deadline allows, so its socket is registered
        // the same way an ordinary connection's is, for `stop` to close.
        let socket_id = shared.next_connection_id();
        let socket = SocketRegistration::new(shared, socket_id, raw_socket);
        state.pairing.held = Some(HeldPairing {
            connection: paired,
            addr,
            accepted,
            _socket: socket,
        });
    }
    shared.set_pairing(&PairingState::Code {
        code,
        expires_unix_secs,
    });
    Ok(())
}

/// Show `Requested` and hold the connection until someone confirms.
///
/// `NegotiatedPending::pair_ik` already checked the handshake's nonce
/// against `expected_nonce` before this runs, so reaching this function at
/// all means some handshake matched the offer. What is still checked here,
/// under the same lock that spends the nonce, is whether another scan
/// already won that race: the same check `hold_pairing` makes for the code
/// method, guarding the same kind of race.
///
/// # Errors
///
/// Returns `Runtime::PairingBusy` when something is already `Requested`, or
/// when pairing has moved on.
fn hold_qr_pairing(
    shared: &Arc<Shared>,
    connection: IkPairedConnection,
    addr: SocketAddr,
) -> Result<(), FerryError> {
    let IkPairedConnection {
        accepted,
        socket: raw_socket,
        ..
    } = connection;
    let name = accepted.name.clone();
    let kind = accepted.kind;
    {
        let mut state = lock(&shared.state);
        if !state.pairing.is_offering() {
            return Err(failed("Runtime::PairingBusy"));
        }
        // The nonce is single use. Spending it here, on the winning side of
        // the race above, is what makes a second scan of the same code
        // refused instead of merely unlucky.
        state.pairing.offer_nonce = None;
        // `docs/engine-contract.md` item 16c: as `hold_pairing`, this
        // connection is held waiting for a confirm, so its socket is
        // registered for `stop` to close.
        let socket_id = shared.next_connection_id();
        let socket = SocketRegistration::new(shared, socket_id, raw_socket);
        state.pairing.requested = Some(RequestedPairing {
            stream: accepted.stream,
            peer: accepted.peer,
            addr,
            name: accepted.name,
            kind,
            hello_done: false,
            _socket: socket,
        });
    }
    shared.set_pairing(&PairingState::Requested {
        name,
        kind: kind.into(),
        transport: Transport::Wifi,
    });
    Ok(())
}

/// Show `Requested` on the side that scanned, and hold the session open.
///
/// The counterpart to [`hold_qr_pairing`], on the other end of the same
/// handshake. The scan proved the static key came from a screen. It did not
/// show the person whose screen, and a person who scanned the wrong code
/// has no way to tell from the handshake alone. The names cross in
/// `exchange_hello`, so this runs after that exchange and publishes the
/// name it carried. `confirm_pairing` then stores the peer, or drops it.
///
/// The session waits in `requested` meanwhile, unread, the same way the
/// offering side's does. Nothing reads it, so the stopping wrapper
/// `hello_with_deadline` put around it has no more work to do and comes off
/// here.
///
/// Does nothing when pairing has already moved on, which is what the
/// watchdog does once the two minute deadline passes.
fn hold_scanned_pairing(
    shared: &Arc<Shared>,
    peer_key: PublicKey,
    stream: StopAware<SecureStream>,
    addr: SocketAddr,
    name: String,
    kind: CoreDeviceKind,
    raw_socket: TcpStream,
) {
    {
        let mut state = lock(&shared.state);
        if state.pairing.requested.is_some() || !state.pairing.is_running() {
            return;
        }
        // `docs/engine-contract.md` item 16c: this side dialled the offer
        // and now holds the connection waiting for a confirm, so its socket
        // is registered too, the same as the offering side's is in
        // `hold_qr_pairing`.
        let socket_id = shared.next_connection_id();
        let socket = SocketRegistration::new(shared, socket_id, raw_socket);
        state.pairing.requested = Some(RequestedPairing {
            stream: stream.into_inner(),
            peer: peer_key,
            addr,
            name: name.clone(),
            kind,
            hello_done: true,
            _socket: socket,
        });
    }
    shared.set_pairing(&PairingState::Requested {
        name,
        kind: kind.into(),
        transport: Transport::Wifi,
    });
}

/// What `confirm_pairing` took out of the pairing state.
pub(crate) enum Confirming {
    /// A code method connection. The names have not crossed yet.
    Code(HeldPairing),
    /// A QR method connection, on either side of the scan.
    Scan(RequestedPairing),
}

/// Carry out an accepted confirm on whichever connection was held.
pub(crate) fn pair_after_confirm(shared: &Arc<Shared>, taken: Confirming) {
    match taken {
        Confirming::Code(held) => finish_pairing(
            shared,
            held.connection.peer,
            held.connection.stream,
            held.addr,
            held.accepted,
            None,
        ),
        Confirming::Scan(RequestedPairing {
            stream,
            peer,
            addr,
            name,
            kind,
            hello_done,
            // The hold is over either way: what follows is either an
            // immediate drop or `finish_pairing`'s own bounded exchange,
            // neither of which is the open-ended wait `_socket` was
            // registered against. `docs/engine-contract.md` item 16c.
            _socket: _,
        }) => {
            if hello_done {
                // The scanning side. The names crossed before `Requested`
                // was shown, so there is nothing left to ask. This side
                // dialed, so it does not serve on this stream either, and
                // letting it go is all that is left to do with it.
                drop(stream);
                store_paired_peer(shared, peer, addr, &name, kind);
            } else {
                // The offering side. Message one carried the other device's
                // hello, and the exchange `finish_pairing` runs must agree
                // with it.
                finish_pairing(shared, peer, stream, addr, true, Some(&(name, kind)));
            }
        }
    }
}

/// Report a failed pairing, unless pairing has already moved on.
fn fail_pairing(shared: &Arc<Shared>, error: FerryError) {
    if !lock(&shared.state).pairing.is_running() {
        return;
    }
    shared.set_pairing(&PairingState::Failed { error });
}

/// Add the network this device is on now to the trusted list.
///
/// `docs/engine-contract.md`, item 18: the first pairing at home trusts
/// home. Called from [`finish_pairing`], which is the one place both pairing
/// methods store a peer, so this covers both methods and both sides.
///
/// An unknown network adds nothing. A full list, or a name this build would
/// refuse, adds nothing either: pairing succeeded, and a list that cannot
/// grow is not a reason to fail it. `Shared::set_pairing` reapplies the rule
/// right after this, when it reports `Confirmed`.
fn trust_current_network(shared: &Arc<Shared>) {
    let Some(name) = lock(&shared.state).network.clone() else {
        return;
    };
    drop(save_networks(shared, |list| list.add(&name)));
}

/// Store the peer, trust the network, and report the new device.
///
/// The second half of every pairing: both methods, both sides. Each side
/// reaches it only after its own person confirmed, so a confirm on one
/// device never stores anything on the other.
///
/// Returns true when it reported `Confirmed`. False means pairing had
/// already ended, or storing failed, and the caller must go no further.
fn store_paired_peer(
    shared: &Arc<Shared>,
    peer_key: PublicKey,
    addr: SocketAddr,
    name: &str,
    kind: CoreDeviceKind,
) -> bool {
    // The watchdog may have given up while the names crossed, or while the
    // person was reading the name. A pairing that already reported Failed
    // must not store a device or report Confirmed after it.
    if !lock(&shared.state).pairing.is_running() {
        return false;
    }

    let key_hex = hex_of(&peer_key);
    if let Err(error) = save_peers(shared, |store| {
        store.add(Peer {
            key: peer_key,
            name: name.to_owned(),
            paired_unix_secs: now_unix_secs(),
            kind,
        })
    }) {
        fail_pairing(shared, error);
        return false;
    }
    trust_current_network(shared);

    let device = {
        let mut state = lock(&shared.state);
        let live = state.live_mut(&key_hex);
        live.last_addr = Some(addr);
        live.last_seen_unix_secs = Some(now_unix_secs());
        state.device(&key_hex)
    };

    let Some(device) = device else {
        fail_pairing(shared, failed("Runtime::NotPaired"));
        return false;
    };
    if !lock(&shared.state).pairing.is_running() {
        return false;
    }
    shared.set_pairing(&PairingState::Confirmed { device });
    notify(shared, Change::Devices);
    true
}

/// Exchange names, store the peer, and report the new device.
///
/// Every path that still has a hello to run: the code method on both sides,
/// and the offering side of a scan. The scanning side ran its hello before
/// it asked its own person, so `pair_after_confirm` calls
/// [`store_paired_peer`] directly for it instead.
///
/// `expected_hello` is the scan's message-one hello, from
/// `RequestedPairing`, when the offering side is what `confirm_pairing`
/// took; `None` for the code method, which has no earlier hello to check
/// against. When it is `Some`, the hello this function exchanges here must
/// agree with it, or the pairing fails: the name and kind shown in
/// `Requested`, that a person already confirmed against, must be the same
/// identity this finishes pairing with.
fn finish_pairing(
    shared: &Arc<Shared>,
    peer_key: PublicKey,
    stream: SecureStream,
    addr: SocketAddr,
    accepted: bool,
    expected_hello: Option<&(String, CoreDeviceKind)>,
) {
    let (name, kind, stream) = match hello_with_deadline(shared, stream) {
        Ok(triple) => triple,
        Err(error) => {
            fail_pairing(shared, error);
            return;
        }
    };
    if let Some((expected_name, expected_kind)) = expected_hello
        && (name != *expected_name || kind != *expected_kind)
    {
        // The scan's message one and this later hello disagree about who
        // this is: treated as an ordinary bad hello, the same code
        // `exchange_hello` itself returns when the first frame is not a
        // hello at all.
        fail_pairing(shared, failed("RpcError::UnexpectedFrameKind"));
        return;
    }

    if !store_paired_peer(shared, peer_key, addr, &name, kind) {
        return;
    }
    let key_hex = hex_of(&peer_key);

    if accepted {
        // The side that accepted keeps serving on this stream. The side that
        // dialed lets it go here: two servers on one stream would each wait
        // for the other to speak.
        //
        // Names were exchanged a few lines above, so this serves the stream
        // as it stands. A second hello would sit waiting for one the other
        // side already sent.
        let allowed = register_serving(shared, &key_hex, addr);
        serve_named_stream(
            shared,
            stream,
            peer_key,
            transport_for_inbound(shared, addr),
            addr,
            name,
            &allowed,
        );
    }
}

/// Exchange names on another thread, and give up after a short wait.
///
/// The other device may confirm slowly, or never. Its stream cannot be woken
/// from outside, so a name exchange that waits inside a read would hold this
/// thread until the idle timeout in `tcp.rs`, five minutes away. `stop`
/// joins this thread, so that was `stop`'s wait too. Running the exchange
/// beside this thread bounds both: the wait ends on its own deadline, and at
/// once when the engine stops.
///
/// The stream is wrapped so that the thread left behind ends quickly as
/// well, instead of holding a socket for five minutes.
///
/// # Errors
///
/// Returns an `RpcError` code when the exchange fails, and
/// `TcpError::Timeout` when it does not finish in time.
fn hello_with_deadline(
    shared: &Arc<Shared>,
    stream: SecureStream,
) -> Result<(String, CoreDeviceKind, StopAware<SecureStream>), FerryError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let my_name = shared.display_name.clone();
    let my_kind = shared.kind;
    let stopping = Arc::clone(&shared.stopping);
    // Not joined. It ends when the exchange ends, or when the wrapper below
    // fails the next read because the engine is stopping.
    //
    // `Builder::spawn` rather than the panicking `thread::spawn`:
    // `docs/audits/fable-engineering.md`, finding 2, names this spawn as
    // the same unguarded pattern `accept_loop`'s had. A refused thread is
    // reported as `FrameError::Io`, the same code a real I/O failure on
    // this exchange would carry, instead of taking the engine down with
    // it.
    let spawned = std::thread::Builder::new().spawn(move || {
        let mut stream = StopAware::new(stream, stopping);
        let outcome =
            exchange_hello(&mut stream, &my_name, my_kind).map(|(name, kind)| (name, kind, stream));
        drop(sender.send(outcome));
    });
    if spawned.is_err() {
        return Err(failed("FrameError::Io"));
    }
    drop(spawned);

    let deadline = Instant::now() + FINISH_PAIRING_DEADLINE;
    loop {
        match receiver.recv_timeout(FINISH_PAIRING_TICK) {
            Ok(Ok(triple)) => return Ok(triple),
            Ok(Err(error)) => return Err(from_rpc(&error)),
            // The thread went away without an answer, which only a panic
            // does. There is no name and no stream to carry on with.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(failed("FrameError::Io"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if shared.stopping() || Instant::now() >= deadline {
                    return Err(failed("TcpError::Timeout"));
                }
            }
        }
    }
}

/// Give up on pairing once the deadline passes.
fn pairing_watchdog(shared: &Arc<Shared>) {
    loop {
        let deadline = lock(&shared.state).pairing.deadline;
        let Some(deadline) = deadline else {
            return;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if !shared.rest(left) {
            return;
        }
        if !lock(&shared.state).pairing.is_running() {
            return;
        }
    }
    // The held connection is taken out under the same lock that reads the
    // state, so a confirm that arrives a moment later finds nothing to
    // confirm.
    let running = {
        let mut state = lock(&shared.state);
        let running = state.pairing.is_running();
        state.pairing.held = None;
        state.pairing.dialing = false;
        state.pairing.offer_nonce = None;
        state.pairing.requested = None;
        running
    };
    if running {
        shared.set_pairing(&PairingState::Failed {
            error: failed("Runtime::PairingTimeout"),
        });
    }
}

/// Atomically checks that no pairing is running and, if not, claims the
/// slot: resets pairing to idle, arms the shared two minute deadline, and
/// starts the watchdog that gives up once it passes. Returns the deadline
/// in Unix seconds, for the caller's own first published state.
///
/// The check and the reset happen under one lock on `shared.state`, held
/// the whole time. `start_pairing_with` and `offer_scanned` used to check
/// `is_running` and then call this as two separate lock acquisitions, which
/// let a second call land in between and start a second pairing attempt of
/// its own; going through this one function closes that gap for both.
///
/// Shared by both of `start_pairing_with`'s branches and by
/// `offer_scanned`'s dial, so "two minutes, one watchdog" stays one fact
/// instead of three copies of it. `docs/engine-contract.md` item 12.
///
/// # Errors
///
/// Returns `None`, with nothing reset, while a pairing is already running.
pub(crate) fn begin_pairing_deadline(shared: &Arc<Shared>) -> Option<i64> {
    let timeout = *lock(&shared.pairing_timeout);
    let expires_unix_secs = now_unix_secs() + i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX);
    {
        let mut state = lock(&shared.state);
        if state.pairing.is_running() {
            return None;
        }
        state.pairing = Pairing::idle();
        state.pairing.deadline = Some(Instant::now() + timeout);
        state.pairing.deadline_unix_secs = Some(expires_unix_secs);
    }
    let watchdog_shared = Arc::clone(shared);
    shared.keep(std::thread::spawn(move || {
        pairing_watchdog(&watchdog_shared);
    }));
    Some(expires_unix_secs)
}

/// This device's non-loopback interface addresses, each paired with `port`.
///
/// Built for a QR pairing offer: the offering device has to state where it
/// can be dialed, and unlike the phone (`ferry_core::discovery::Advertiser`)
/// it never advertises over mDNS, so there is no existing list of its own
/// addresses to read back. `if-addrs` enumerates the system's network
/// interfaces directly instead.
///
/// This build does not try to tell a Wi-Fi interface apart from any other
/// kind by name or platform API; a Mac used to show a pairing QR code has
/// one non-loopback, non-link-local interface worth offering in the
/// ordinary case, and a finer distinction is future work.
///
/// Two kinds of address are excluded outright, not merely deprioritised.
/// Loopback, since neither Wi-Fi nor the cable is ever a loopback address,
/// and a phone could not dial one anyway. Link-local (`169.254.0.0/16` and
/// `fe80::/10`), since a link-local address is only meaningful together
/// with the interface it came from, and the offer's wire format
/// (`ferry_core::offer`) has no field for that interface index: an
/// unqualified link-local address is not merely low priority, it is
/// ambiguous, and a real machine hands back several of them at once, on
/// tunnel and peer-to-peer interfaces nobody is dialing over. Trying to
/// connect to one anyway does not fail fast; the OS holds the attempt open,
/// so a handful of them ahead of the one real address in the list can cost
/// most of a minute before `dial_offer` ever reaches it.
///
/// A machine with no usable interface gets an offer with zero addresses;
/// the phone that scans it fails to dial any and reports
/// `Runtime::NotReachable`, the same as it would for a paired device that
/// dropped off the network.
///
/// Capped at [`ferry_core::offer::MAX_DIAL_ADDRESSES`], private-range
/// addresses first, by [`ferry_core::offer::dialable_addresses`]: the same
/// policy `dial_offer` applies to a scanned offer's own list, since a
/// machine with many interfaces should not draw a QR code that makes a
/// phone try dialing all of them.
fn local_wifi_addresses(port: u16) -> Vec<SocketAddr> {
    let addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| SocketAddr::new(interface.ip(), port));
    ferry_core::offer::dialable_addresses(addresses)
}

/// Make a QR offer and publish `Offering`, or `Failed` if the system has no
/// randomness to make a nonce with.
///
/// `expires_unix_secs` is the deadline `start_pairing_with` already
/// claimed through `begin_pairing_deadline`, atomically with its running
/// check: by the time this runs, pairing is already reset to idle and the
/// watchdog is already racing against that deadline. A failure here still
/// reports `Failed` correctly: `set_pairing` clears the deadline the
/// watchdog is waiting on once pairing is no longer running.
pub(crate) fn start_offering(shared: &Arc<Shared>, expires_unix_secs: i64) {
    let mut nonce = [0u8; QR_NONCE_LEN];
    if getrandom::fill(&mut nonce).is_err() {
        shared.set_pairing(&PairingState::Failed {
            error: failed("Runtime::NoRandomness"),
        });
        return;
    }

    let port = lock(&shared.net)
        .as_ref()
        .map_or(shared.listen_port, |net| net.local_addr().port());
    let offer = Offer {
        version: 1,
        static_key: shared.key.public(),
        expires_unix_secs,
        nonce,
        addresses: local_wifi_addresses(port),
    };
    lock(&shared.state).pairing.offer_nonce = Some(nonce);
    shared.set_pairing(&PairingState::Offering {
        offer: PairingOffer {
            payload: offer.encode(),
            expires_unix_secs,
        },
    });
}

/// Dial a scanned offer's addresses in order, run `IK` as the initiator,
/// exchange names, and then ask this side's own person about the name that
/// came back, through [`hold_scanned_pairing`]. `docs/engine-contract.md`
/// item 12: both sides confirm by name, and each stores only after its own
/// confirm.
///
/// `offer.addresses` came from a scanned QR code, so it is not trusted as
/// bounded or ordered: it is filtered the same way `local_wifi_addresses`
/// filters this device's own, through
/// [`ferry_core::offer::dialable_addresses`], before a single address is
/// dialed. A real offer's addresses always survive that filter already,
/// since `local_wifi_addresses` is what produced them; reaching an offer
/// whose every address is loopback or link-local means something unusual,
/// not a hostile flood, since the cap below still bounds that case the
/// same as any other, so the un-filtered list is tried instead of dialing
/// nothing. The loop also gives up as soon as `stop` begins, so a `stop`
/// that lands while this is dialing an unreachable address does not wait
/// for every remaining one first.
pub(crate) fn dial_offer(shared: &Arc<Shared>, offer: &Offer) {
    let filtered = ferry_core::offer::dialable_addresses(offer.addresses.iter().copied());
    let addresses = if filtered.is_empty() {
        offer
            .addresses
            .iter()
            .copied()
            .take(ferry_core::offer::MAX_DIAL_ADDRESSES)
            .collect()
    } else {
        filtered
    };
    let mut last_error = None;
    for addr in &addresses {
        if shared.stopping() {
            return;
        }
        match tcp::pair_ik(
            *addr,
            &shared.key,
            &offer.static_key,
            &offer.nonce,
            &shared.display_name,
            shared.kind,
        ) {
            Ok((stream, socket)) => {
                // Names cross, and then this side asks its own person about
                // the name it got; see `hold_scanned_pairing`, which is
                // where `socket` is registered, once the connection is
                // actually held waiting for that confirm.
                let (name, kind, stream) = match hello_with_deadline(shared, stream) {
                    Ok(triple) => triple,
                    Err(error) => {
                        fail_pairing(shared, error);
                        return;
                    }
                };
                hold_scanned_pairing(shared, offer.static_key, stream, *addr, name, kind, socket);
                return;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if shared.stopping() {
        return;
    }
    let error = last_error.map_or_else(|| failed("Runtime::NotReachable"), |e| from_tcp(&e));
    fail_pairing(shared, error);
}

/// The stored peers to offer `Pending::connect` as candidates, in the order
/// a responder should try them: the one whose last known address matches
/// `remote` first, then the rest.
///
/// The wire says nothing about who is calling before the handshake, so every
/// stored peer is a candidate; `Pending::connect` reads message one once and
/// tries each in turn (`docs/engine-contract.md` item 16b). Storage already
/// refuses more than `peers::MAX_PEERS` (64) peers, which already bounds how
/// many are ever tried here.
///
/// A device with no stored peer gets exactly one candidate, a key nobody
/// holds, so a stranger sees the same shape of failure whether or not this
/// device has ever paired.
pub(crate) fn candidate_peers(shared: &Arc<Shared>, remote: SocketAddr) -> Vec<PublicKey> {
    let state = lock(&shared.state);
    let mut peers = state.peers.all();
    if peers.is_empty() {
        return vec![nobody(shared)];
    }
    peers.sort_by_key(|peer| {
        let matches_address = state
            .live
            .get(&hex_of(&peer.key))
            .and_then(|live| live.last_addr)
            .is_some_and(|addr| addr.ip() == remote.ip());
        // `false` sorts before `true`, so a matching address comes first.
        !matches_address
    });
    peers.into_iter().map(|peer| peer.key).collect()
}
