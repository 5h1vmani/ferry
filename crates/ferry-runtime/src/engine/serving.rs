//! Serving one paired peer over an established connection.

use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ferry_core::limits::MAX_SERVING_PER_PEER;
use ferry_core::noise::PublicKey;
use ferry_core::peers::Peer;
use ferry_core::rpc::{exchange_hello, serve};
use ferry_core::tcp::Connection;

use crate::Transport;
use crate::guard::GuardedFs;
use crate::notify::Change;
use crate::state::{hex_of, lock, now_unix_secs};

use super::{Shared, mark_reachable, notify, save_peers, transport_for_inbound};

/// Put this connection's off switch where `forget` and `stop` can reach it,
/// unconditionally.
///
/// This happens as soon as the handshake proves who is calling, and before
/// the names are exchanged. A peer that finishes the handshake and then
/// holds its `hello` back for five minutes would otherwise be out of reach:
/// `forget` would find no switch to flip, and the connection would start
/// serving files afterwards.
///
/// The switch starts off if the engine is already stopping, so a connection
/// that arrives during `stop` serves nothing.
///
/// `finish_pairing` calls this directly, uncapped: its own serving
/// connection is the first one this peer could ever have, immediately after
/// pairing confirms, so [`MAX_SERVING_PER_PEER`] can never apply to it.
/// [`serve_connection`], reached from an ordinary `Mode::Connect` dial,
/// calls [`try_register_serving`] instead, which is where finding 5's cap
/// is actually enforced.
pub(crate) fn register_serving(
    shared: &Arc<Shared>,
    key_hex: &str,
    addr: SocketAddr,
) -> Arc<AtomicBool> {
    register_serving_inner(shared, key_hex, addr, false)
        .expect("the cap is not enforced here, so this always reserves a switch")
}

/// As [`register_serving`], but refuses once this peer already holds
/// [`MAX_SERVING_PER_PEER`] serving connections.
///
/// `docs/audits/fable-security.md`, finding 5: a paired device that opened
/// more serving connections than it ever read from held one thread each,
/// forever, since nothing capped how many it could hold at once at all.
/// Refusing here, before a socket is registered or a name exchanged, means
/// a refused connection costs this device nothing beyond the accept itself.
fn try_register_serving(
    shared: &Arc<Shared>,
    key_hex: &str,
    addr: SocketAddr,
) -> Option<Arc<AtomicBool>> {
    register_serving_inner(shared, key_hex, addr, true)
}

/// What [`register_serving`] and [`try_register_serving`] share: reserving
/// one serving switch under one lock, so the check and the push can never
/// race against another thread doing the same for this peer. `enforce_cap`
/// is `false` only for `register_serving`'s own caller.
fn register_serving_inner(
    shared: &Arc<Shared>,
    key_hex: &str,
    addr: SocketAddr,
    enforce_cap: bool,
) -> Option<Arc<AtomicBool>> {
    let mut state = lock(&shared.state);
    let live = state.live_mut(key_hex);
    if enforce_cap && live.serving.len() >= MAX_SERVING_PER_PEER as usize {
        return None;
    }
    let allowed = Arc::new(AtomicBool::new(!shared.stopping()));
    live.last_addr = Some(addr);
    live.serving.push(Arc::clone(&allowed));
    Some(allowed)
}

/// Exchange names, then serve the shared root until the connection ends.
pub(crate) fn serve_connection(shared: &Arc<Shared>, connection: Connection, peer: PublicKey) {
    let transport = transport_for_inbound(shared, connection.remote);
    let key_hex = hex_of(&peer);
    let Some(allowed) = try_register_serving(shared, &key_hex, connection.remote) else {
        // `docs/audits/fable-security.md`, finding 5: this peer already
        // holds `MAX_SERVING_PER_PEER` connections. `connection` drops here,
        // closing its socket at once; nothing is reported, the same as any
        // other refusal before a name is exchanged.
        return;
    };
    let socket_id = shared.next_connection_id();
    let _socket = SocketRegistration::new(shared, socket_id, connection.socket);
    serve_stream(
        shared,
        connection.stream,
        peer,
        transport,
        connection.remote,
        &allowed,
    );
}

/// Keeps one connection's raw socket registered in `Shared`, for `stop` to
/// close directly, for as long as this value lives.
///
/// `docs/engine-contract.md` item 16c: every connection registers when it is
/// established and removes itself when it ends. Using a guard, instead of a
/// bare register-then-unregister pair, means an early return or a panic on
/// any path still frees the entry.
///
/// Owns a clone of `Arc<Shared>`, rather than borrowing one, so this can
/// live inside state that outlives the stack frame that created it: a code
/// or QR pairing holds its connection in `Pairing::held` or
/// `Pairing::requested`, inside `Shared` itself, for as long as a person
/// takes to compare a code or confirm a name, so a borrowed reference could
/// not be stored there.
pub(crate) struct SocketRegistration {
    shared: Arc<Shared>,
    id: u64,
}

impl SocketRegistration {
    pub(crate) fn new(shared: &Arc<Shared>, id: u64, socket: TcpStream) -> Self {
        shared.register_socket(id, socket);
        Self {
            shared: Arc::clone(shared),
            id,
        }
    }
}

impl Drop for SocketRegistration {
    fn drop(&mut self) {
        self.shared.unregister_socket(self.id);
    }
}

/// Serve the shared root on one stream, and keep the device list honest.
fn serve_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    addr: SocketAddr,
    allowed: &Arc<AtomicBool>,
) {
    let Ok((name, _kind)) = exchange_hello(&mut stream, &shared.display_name, shared.kind) else {
        release_serving(shared, &hex_of(&peer), allowed);
        return;
    };
    serve_named_stream(shared, stream, peer, transport, addr, name, allowed);
}

/// Serve the shared root on a stream whose names were already exchanged.
pub(crate) fn serve_named_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    addr: SocketAddr,
    name: String,
    allowed: &Arc<AtomicBool>,
) {
    let key_hex = hex_of(&peer);
    let renamed = {
        let state = lock(&shared.state);
        // The handshake proved that the caller held a paired key at that
        // moment. `forget` may have run in the short gap before this
        // connection's switch was registered, and a switch registered after
        // that starts on. So the list is asked once more here, now that the
        // switch is in place for any later `forget` to flip.
        let Some(stored) = state.peers.get(&peer) else {
            drop(state);
            release_serving(shared, &key_hex, allowed);
            return;
        };
        let name_changed = stored.name != name;
        let paired_unix_secs = stored.paired_unix_secs;
        let kind = stored.kind;
        // A name the peer changed since pairing is stored, so the list stays
        // current without another pairing. Its kind does not change here: it
        // was set once, from the hello sent at pairing time.
        name_changed.then_some(Peer {
            key: peer,
            name,
            paired_unix_secs,
            kind,
        })
    };
    // docs/engine-contract.md, batch B, item 3: the dialling side already
    // calls `mark_reachable` before it serves a connection; the accepting
    // side never did, so it never recorded a Wi-Fi success and
    // `available_transports` never listed `Wifi` for the caller once the
    // connection ended. Calling the same path here records it on both
    // transports, the same way the dialling side does.
    mark_reachable(shared, &key_hex, addr, transport);
    if let Some(peer) = renamed {
        // Outside the lock. Writing the list calls `fsync`, and any peer can
        // ask for this by sending a name of its own choosing.
        drop(save_peers(shared, |store| store.add(peer)));
    }
    notify(shared, Change::Devices);

    if lock(&shared.roots).is_none() {
        release_serving(shared, &key_hex, allowed);
        return;
    }
    // `GuardedFs` holds this handle, not a snapshot, so a `set_roots` call
    // reaches this connection on its very next operation.
    let connection = shared.next_connection_id();
    let guarded = GuardedFs::new(
        Arc::clone(&shared.roots),
        Arc::clone(allowed),
        Arc::clone(&shared.access_log),
        connection,
        key_hex.clone(),
    );
    // A connection that ends is the ordinary outcome. The error, if any, has
    // nowhere useful to go: the person did not ask for this connection.
    drop(serve(&mut stream, &guarded));

    // docs/engine-contract.md, item 13: a served connection ending is what
    // finalises whatever it was still in the middle of.
    if let Some(rollup) = lock(&shared.access_log).as_mut() {
        rollup.connection_ended(now_unix_secs(), connection);
    }

    release_serving(shared, &key_hex, allowed);
    notify(shared, Change::Devices);
}

/// Take this connection's switch back once it has finished.
fn release_serving(shared: &Arc<Shared>, key_hex: &str, allowed: &Arc<AtomicBool>) {
    let mut state = lock(&shared.state);
    let live = state.live_mut(key_hex);
    live.reachable_via = None;
    live.last_seen_unix_secs = Some(now_unix_secs());
    live.serving.retain(|switch| !Arc::ptr_eq(switch, allowed));
}
