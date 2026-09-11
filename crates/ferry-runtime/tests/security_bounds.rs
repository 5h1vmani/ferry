//! Regression tests for the fix pass over `docs/audits/fable-security.md`,
//! findings 1, 2, 4, 5, 6 and 7.
//!
//! One file, per `docs/agent-runs.md` rule 3: every test this fix pass adds
//! lives here, not appended to a shared file. The peer-and-engine harness
//! comes from `tests/common/paths.rs`; `build_with_peers`, `is_waiting`,
//! and `pair_by_code` below are specific to what these tests need beyond
//! it.
//!
//! Finding 2's own test is not here. `AccessLog`, the type its per-device
//! day cap lives on, is `pub(crate)` in `ferry-runtime`, so an external test
//! binary such as this one cannot reach it; going through the public engine
//! API instead would mean tens of thousands of real file operations over a
//! real connection, which is not the cheap test `docs/agent-runs.md` rule 8
//! asks for. That test is a unit test inside
//! `crates/ferry-runtime/src/access.rs`'s own `#[cfg(test)]` module instead,
//! next to the store it tests.
//!
//! Finding 6 is a documentation fix, in `docs/protocol.md`, with no code and
//! so no test.
//!
//! Finding 7's test needs a running `WebDAV` bridge, which
//! `tests/common/mod.rs` already builds for the `dav_*.rs` files; `mod common`
//! below reuses it rather than copying it a third time.

mod common;

use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_core::peers::DeviceKind as CoreDeviceKind;
use ferry_core::rpc::exchange_hello;
use ferry_core::tcp;
use ferry_runtime::{
    DeviceKind as RuntimeDeviceKind, KeyPair, PairingMethod, PairingState, generate_key,
};

use common::paths::{
    Inbox, Side, build, is_code, is_confirmed, is_found, loopback_addr, make_engine, public_key,
    static_key,
};
use common::{TestClient, build_side, loopback_addr as dav_loopback_addr, port_of};

/// As [`build`], but seeds the engine's own peer store with `peer_keys`
/// first, so each is already paired without running the pairing handshake.
/// Only the inbound-connection-cap test below needs more than one stored
/// peer at once.
fn build_with_peers(name: &str, peer_keys: &[KeyPair]) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key = generate_key().expect("a fresh key pair");
    for (n, peer_key) in peer_keys.iter().enumerate() {
        common::seed_peer(
            data.path(),
            peer_key,
            &format!("Peer {n}"),
            RuntimeDeviceKind::Phone,
        );
    }
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        name,
        key.clone(),
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    )
    .expect("the engine should build from a good config");
    engine.start().expect("the engine should start");
    Side {
        engine,
        inbox,
        key,
        data,
        shared,
        download,
    }
}

fn is_waiting(state: &PairingState) -> bool {
    matches!(state, PairingState::Waiting { .. })
}

/// Pair `a` and `b` by code, over their real listeners, with `a` as the side
/// that waits and `b` as the side that looks and dials in.
fn pair_by_code(a: &Side, b: &Side) {
    a.engine.set_reachable(true);
    a.engine.start_pairing_with(PairingMethod::Code);
    a.inbox.wait_pairing("the waiting side to wait", is_waiting);

    let a_addr = loopback_addr(a);
    b.engine.start_pairing_with(PairingMethod::Code);
    b.engine.offer_candidate(a_addr);
    let found = b.inbox.wait_pairing("a candidate", is_found);
    let PairingState::Found { candidates, .. } = &found else {
        panic!("expected candidates, got {found:?}");
    };
    let wanted = format!("wifi:{a_addr}");
    let chosen = candidates
        .iter()
        .find(|candidate| candidate.id == wanted)
        .unwrap_or_else(|| panic!("the injected candidate {wanted} should be listed"));
    b.engine
        .pick_candidate(chosen.id.clone())
        .expect("the candidate should be pickable");

    a.inbox.wait_pairing("the waiting side's code", is_code);
    b.inbox.wait_pairing("the dialling side's code", is_code);
    a.engine.confirm_pairing(true);
    b.engine.confirm_pairing(true);
    a.inbox
        .wait_pairing("the waiting side to confirm", is_confirmed);
    b.inbox
        .wait_pairing("the dialling side to confirm", is_confirmed);
}

// ---------------------------------------------------------------------------
// Findings 1 and 4: a two second first-byte deadline, and a per-address cap
// on pending handshakes.
// ---------------------------------------------------------------------------

/// A connection that sends nothing is dropped once
/// `ferry_core::limits::FIRST_BYTE_TIMEOUT_SECS` passes, and a real device
/// can still pair right after it.
///
/// Before the fix, `Pending::negotiate` read nothing until the whole ten
/// second `HANDSHAKE_TIMEOUT_SECS` handshake deadline, so this test's first
/// `read` timed out waiting on the silent connection instead of seeing it
/// closed.
#[test]
fn a_silent_connection_is_dropped_at_the_first_byte_deadline_and_pairing_still_works() {
    let target = build("Target");
    target.engine.set_reachable(true);
    target.engine.start_pairing_with(PairingMethod::Code);
    target
        .inbox
        .wait_pairing("the target to wait for pairing", is_waiting);
    let addr = loopback_addr(&target);

    let mut silent = TcpStream::connect(addr).expect("the accept itself is never refused");
    // A margin over the two second deadline, never over it by much: rule 8
    // in `docs/agent-runs.md` asks for no test waiting past the deadline
    // plus a margin.
    silent
        .set_read_timeout(Some(Duration::from_secs(4)))
        .expect("a read timeout can be set");
    let start = Instant::now();
    let mut buf = [0u8; 1];
    let read = silent.read(&mut buf);
    let elapsed = start.elapsed();
    match read {
        Ok(0) => {}
        other => panic!("expected the silent connection to be closed, got {other:?}"),
    }
    assert!(
        elapsed >= Duration::from_millis(1_800),
        "the silent connection was dropped too early, after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "the silent connection should be dropped near the two second deadline, took {elapsed:?}"
    );

    // A real device dialling in right after must still pair normally: the
    // silent connection above must not have used up a slot the real one
    // needed, or left the listener otherwise unhappy.
    let real_peer = build("Peer");
    pair_by_code(&target, &real_peer);
    assert_eq!(target.engine.devices().len(), 1, "the target paired");
    assert_eq!(real_peer.engine.devices().len(), 1, "the peer paired");
}

/// One pending connection past `ferry_core::limits::MAX_PENDING_HANDSHAKES_PER_ADDR`
/// is refused while that many from the same source address are still held.
///
/// Before the fix, only the overall `MAX_PENDING_HANDSHAKES` cap existed,
/// so every connection from one address was accepted and this test's last
/// connection would not have closed at once.
///
/// The count is read from the constant rather than written here, so raising
/// the cap does not leave this test proving something smaller than the cap
/// it is named for.
#[test]
fn one_pending_connection_past_the_per_address_cap_is_refused() {
    let target = build("Target");
    target.engine.set_reachable(true);
    let addr = loopback_addr(&target);

    // None of these sends a byte, so each sits waiting on the first-byte
    // deadline, holding its pending slot for up to two seconds: plenty of
    // time to check the one below well inside that window.
    let held: Vec<TcpStream> = (0..ferry_core::limits::MAX_PENDING_HANDSHAKES_PER_ADDR)
        .map(|n| {
            TcpStream::connect(addr)
                .unwrap_or_else(|error| panic!("connection {n} should be accepted: {error}"))
        })
        .collect();

    let mut one_too_many = TcpStream::connect(addr).expect("the accept itself is never refused");
    one_too_many
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("a read timeout can be set");
    let mut buf = [0u8; 1];
    match one_too_many.read(&mut buf) {
        Ok(0) => {}
        other => panic!(
            "expected the connection past the per-address cap to be refused at once, got {other:?}"
        ),
    }
    drop(held);
}

// ---------------------------------------------------------------------------
// Finding 5: a cap on serving connections per paired peer.
// ---------------------------------------------------------------------------

/// A ninth serving connection from one paired peer is refused, per
/// `ferry_core::limits::MAX_SERVING_PER_PEER`, while the first eight are
/// still served normally.
///
/// Before the fix, `register_serving` pushed to `live.serving` with no
/// cap at all, so a ninth connection from the same paired key would have
/// been accepted exactly like the first.
///
/// The friend is a seeded key, not a second running engine. A running
/// engine browses mDNS, and item 3's reachability probe turns a discovery
/// event into one pooled connection to each paired device, which would
/// take one of the eight serving slots this test counts. Seeding the key
/// pairs it as far as this cap is concerned and leaves nothing running to
/// dial in.
#[test]
fn a_ninth_serving_connection_from_one_peer_is_refused() {
    let friend_keys = [generate_key().expect("a fresh key pair")];
    let target = build_with_peers("Target", &friend_keys);
    target.engine.set_reachable(true);

    let addr = loopback_addr(&target);
    let friend_key = static_key(&friend_keys[0]);
    let target_public = public_key(&target.key);

    // Each of the first `MAX_SERVING_PER_PEER` connections, reconnecting as
    // the already-paired friend the way `tests/common/paths.rs`'s hand-built
    // peers do, must be served: the handshake succeeds and the name
    // exchange that only a served connection answers completes.
    let mut held = Vec::new();
    for n in 0..ferry_core::limits::MAX_SERVING_PER_PEER {
        let connection = tcp::connect(addr, &friend_key, &target_public)
            .unwrap_or_else(|error| panic!("connection {n} should be accepted: {error}"));
        let mut stream = connection.stream;
        exchange_hello(&mut stream, "Friend", CoreDeviceKind::Phone)
            .unwrap_or_else(|error| panic!("connection {n} should be served: {error}"));
        held.push(stream);
    }

    // The Noise handshake itself is not capped, only serving is, so this
    // connects fine and then gets nothing back: the name exchange waits on
    // an answer `serve_connection` never sends, because it dropped the
    // connection before registering it, and so the read fails once the
    // socket closes.
    let ninth = tcp::connect(addr, &friend_key, &target_public)
        .expect("the Noise handshake itself is not capped");
    let mut ninth_stream = ninth.stream;
    match exchange_hello(&mut ninth_stream, "Friend", CoreDeviceKind::Phone) {
        Err(_) => {}
        Ok(_) => panic!("the ninth serving connection should have been refused"),
    }

    drop(held);
}

// ---------------------------------------------------------------------------
// Finding 7: the local WebDAV bridge's first-head timeout and its cap on
// unauthenticated connections.
// ---------------------------------------------------------------------------

/// A fifth silent connection to the bridge is refused at once, and once the
/// first four have timed out and freed their slots, a real, authenticated
/// connection is still served normally.
///
/// Before the fix, `ferry_runtime::dav::server` capped only the overall
/// thirty-two live connections, so a fifth silent one would have been
/// accepted the same as the first four, and held for the full thirty
/// second `CONNECTION_TIMEOUT` instead of the five second
/// `FIRST_HEAD_TIMEOUT` this fix adds.
#[test]
fn silent_connections_to_the_bridge_do_not_stop_an_authenticated_one() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Mac",
        RuntimeDeviceKind::Mac,
        mac_key.clone(),
        &[],
        &phone_key,
        "Phone",
        RuntimeDeviceKind::Phone,
    );
    let phone = build_side(
        "Phone",
        RuntimeDeviceKind::Phone,
        phone_key.clone(),
        &[("note.txt", b"hi")],
        &mac_key,
        "Mac",
        RuntimeDeviceKind::Mac,
    );
    phone.engine.set_reachable(true);
    let phone_key_hex = mac
        .engine
        .devices()
        .first()
        .expect("build_side seeds each side with the other as a peer")
        .key_hex
        .clone();
    mac.engine.offer_candidate(dav_loopback_addr(&phone));
    mac.engine
        .list(phone_key_hex.clone(), String::new())
        .expect("listing should succeed, so the pool has a working connection");

    let endpoint = mac
        .engine
        .mount_start(phone_key_hex)
        .expect("mount_start should succeed once the phone is reachable");
    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");
    let host = format!("127.0.0.1:{}", port_of(&endpoint.url));

    // Four silent connections fill MAX_UNAUTHENTICATED_CONNECTIONS. A fifth
    // must be refused at once: this is the assertion that fails without
    // the fix, since nothing capped unauthenticated connections separately
    // from the overall MAX_LIVE_CONNECTIONS of 32 before it.
    let mut silent = Vec::new();
    for _ in 0..4 {
        silent.push(TcpStream::connect(addr).expect("the bridge should accept up to the cap"));
    }
    // A generous margin for the accept loop's own thread to reserve each
    // slot; ordering alone already guarantees it happens before the fifth
    // is looked at.
    std::thread::sleep(Duration::from_millis(200));
    let mut fifth =
        TcpStream::connect(addr).expect("the accept itself is never refused, only serving is");
    fifth
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("a read timeout can be set");
    let mut buf = [0u8; 1];
    match fifth.read(&mut buf) {
        Ok(0) => {}
        other => {
            panic!("expected the fifth silent connection to be refused at once, got {other:?}")
        }
    }

    // The four squatters above go silent past FIRST_HEAD_TIMEOUT, freeing
    // their slots. This waits past that five second deadline, with a
    // margin, but nowhere near the bridge's own thirty second
    // CONNECTION_TIMEOUT, the number this fix pass shortened it from.
    std::thread::sleep(Duration::from_secs(6));

    let mut client = TestClient::connect(addr);
    let response = client.request(
        "OPTIONS",
        "/",
        &host,
        Some((&endpoint.user, &endpoint.password)),
        &[],
        None,
    );
    assert_eq!(
        response.status, 200,
        "an authenticated request should still be served once the squatters time out"
    );

    drop(silent);
    drop(fifth);
    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// `docs/audits/fable-engineering.md`, finding 2: a cap on inbound
// connections, across every peer.
// ---------------------------------------------------------------------------

/// The sixty-fifth inbound connection is refused while sixty-four, each a
/// different paired peer's, are still open, per
/// `ferry_core::limits::MAX_INBOUND_CONNECTIONS`. Once all sixty-four close,
/// the accept loop still accepts normally.
///
/// Before the fix, `accept_loop` capped nothing beyond the pending-handshake
/// limits `ferry_core::tcp::Listener` already enforces, and those only bound
/// a connection before its own handshake finished. Sixty-four connections,
/// each already past that point and each its own paired peer so
/// `MAX_SERVING_PER_PEER` (finding 5) never touches them, would not have
/// stopped a sixty-fifth from being accepted and served too.
///
/// Held past their own handshake rather than during it: `net.accept`'s own
/// `MAX_PENDING_HANDSHAKES`, thirty-two overall, would refuse a connection
/// still mid-handshake long before this cap's own sixty-four could ever be
/// reached, on this or any other test that tried to hold that many
/// connections still negotiating at once. Each of the sixty-four below
/// finishes its handshake and its name exchange, the way a real serving
/// connection would, before the next one dials.
#[test]
fn the_sixty_fifth_inbound_connection_is_refused_while_sixty_four_are_held() {
    let peer_keys: Vec<KeyPair> = (0..ferry_core::limits::MAX_INBOUND_CONNECTIONS)
        .map(|_| generate_key().expect("a fresh key pair"))
        .collect();
    let target = build_with_peers("Target", &peer_keys);
    target.engine.set_reachable(true);
    let addr = loopback_addr(&target);
    let target_public = public_key(&target.key);

    // Each connects as its own already-paired peer and is served: the name
    // exchange only a served connection answers completes. None sends
    // another request after that, so each holds its inbound slot, and its
    // thread, open until this test drops it.
    let mut held = Vec::with_capacity(peer_keys.len());
    for (n, peer_key) in peer_keys.iter().enumerate() {
        let dialing_key = static_key(peer_key);
        let connection = tcp::connect(addr, &dialing_key, &target_public)
            .unwrap_or_else(|error| panic!("connection {n} should be accepted: {error}"));
        let mut stream = connection.stream;
        exchange_hello(&mut stream, &format!("Peer {n}"), CoreDeviceKind::Phone)
            .unwrap_or_else(|error| panic!("connection {n} should be served: {error}"));
        held.push(stream);
    }

    // The sixty-fifth needs no identity of its own: the cap is checked in
    // `accept_loop`, before any handshake runs, so a plain silent
    // connection is refused the same way a real one would be.
    let mut sixty_fifth = TcpStream::connect(addr).expect("the accept itself is never refused");
    sixty_fifth
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("a read timeout can be set");
    let mut buf = [0u8; 1];
    match sixty_fifth.read(&mut buf) {
        Ok(0) => {}
        other => panic!(
            "expected the sixty-fifth inbound connection to be refused at once, got {other:?}"
        ),
    }

    // The cap is a live count, not something that latches once tripped: once
    // the sixty-four holders close, the accept loop must accept and serve a
    // fresh connection normally again.
    drop(held);
    // A generous margin for each of the sixty-four served threads to notice
    // its socket closed and free its slot.
    std::thread::sleep(Duration::from_millis(300));
    let reconnect_key = static_key(&peer_keys[0]);
    let mut reconnected = tcp::connect(addr, &reconnect_key, &target_public)
        .expect("the accept loop should still accept once the sixty-four holders have closed")
        .stream;
    exchange_hello(&mut reconnected, "Peer 0", CoreDeviceKind::Phone)
        .expect("a fresh connection should be served normally again");
}

// ---------------------------------------------------------------------------
// Helpers used only from here down are added as later findings are fixed.
// ---------------------------------------------------------------------------
