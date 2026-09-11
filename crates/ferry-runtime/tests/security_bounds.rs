//! Regression tests for the fix pass over `docs/audits/fable-security.md`,
//! findings 1, 2, 4, 5, 6 and 7.
//!
//! One file, per `docs/agent-runs.md` rule 3: every test this fix pass adds
//! lives here, not appended to `engine_paths.rs` or any other shared file.
//! The helpers below are copied from `engine_paths.rs` and trimmed to what
//! these tests use.
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
//! `tests/common/mod.rs` already builds for `tests/dav.rs`; `mod common`
//! below reuses it rather than copying it a third time.

mod common;

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::peers::DeviceKind as CoreDeviceKind;
use ferry_core::rpc::exchange_hello;
use ferry_core::tcp;
use ferry_runtime::{
    Config, DeviceKind as RuntimeDeviceKind, Engine, EngineListener, FerryError, KeyPair,
    PairingMethod, PairingState, Root, generate_key,
};

use common::{TestClient, build_side, loopback_addr as dav_loopback_addr, port_of};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// The listener one engine is given. Copied from `engine_paths.rs`.
// ---------------------------------------------------------------------------

/// What one engine has told the app so far.
#[derive(Default)]
struct Notes {
    /// Every pairing state, in the order it arrived.
    pairings: Vec<PairingState>,
}

/// Collects callbacks and lets the test wait for one.
#[derive(Default)]
struct Inbox {
    notes: Mutex<Notes>,
    ready: Condvar,
}

impl Inbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Notes> {
        self.notes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn pairing(&self, state: PairingState) {
        let mut notes = self.lock();
        notes.pairings.push(state);
        drop(notes);
        self.ready.notify_all();
    }

    /// Wait until a pairing state that `want` accepts has arrived.
    fn wait_pairing(&self, what: &str, want: impl Fn(&PairingState) -> bool) -> PairingState {
        let deadline = Instant::now() + PATIENCE;
        let mut notes = self.lock();
        loop {
            if let Some(found) = notes.pairings.iter().find(|state| want(state)) {
                return found.clone();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "waited {PATIENCE:?} for {what}");
            let (next, _) = self
                .ready
                .wait_timeout(notes, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            notes = next;
        }
    }
}

/// The listener one engine is given.
struct Recorder {
    inbox: Arc<Inbox>,
}

impl EngineListener for Recorder {
    fn devices_changed(&self) {}
    fn transfers_changed(&self) {}

    fn pairing_changed(&self, state: PairingState) {
        self.inbox.pairing(state);
    }

    fn access_log_changed(&self) {}
}

// ---------------------------------------------------------------------------
// Engines under test. Copied from `engine_paths.rs`.
// ---------------------------------------------------------------------------

/// One engine, its inbox, and the folders it owns.
///
/// None of the tests below reads `_data`, `_shared`, or `_download`
/// directly; each field only has to outlive the engine, so its temporary
/// folder is not deleted while the engine still serves from it.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    key: KeyPair,
    _data: tempfile::TempDir,
    _shared: tempfile::TempDir,
    _download: tempfile::TempDir,
}

/// Build an engine on the given folders. It is not started.
fn make_engine(
    name: &str,
    key: KeyPair,
    data: &Path,
    shared: &Path,
    download: &Path,
    inbox: &Arc<Inbox>,
) -> Result<Arc<Engine>, FerryError> {
    Engine::new(
        Config {
            data_dir: data.to_string_lossy().into_owned(),
            shared_roots: vec![Root {
                name: "Root".to_owned(),
                path: shared.to_string_lossy().into_owned(),
                writable: true,
            }],
            download_dir: download.to_string_lossy().into_owned(),
            display_name: name.to_owned(),
            listen_port: 0,
            key,
            kind: RuntimeDeviceKind::Mac,
        },
        Box::new(Recorder {
            inbox: Arc::clone(inbox),
        }),
    )
}

/// Build and start one engine on fresh folders.
fn build(name: &str) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key = generate_key().expect("a fresh key pair");
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
        _data: data,
        _shared: shared,
        _download: download,
    }
}

/// The address another engine, or a raw client, in this process can dial.
fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
}

fn is_waiting(state: &PairingState) -> bool {
    matches!(state, PairingState::Waiting { .. })
}

fn is_found(state: &PairingState) -> bool {
    matches!(state, PairingState::Found { .. })
}

fn is_code(state: &PairingState) -> bool {
    matches!(state, PairingState::Code { .. })
}

fn is_confirmed(state: &PairingState) -> bool {
    matches!(state, PairingState::Confirmed { .. })
}

/// Rebuild the Noise key from the bytes the app would have stored. Copied
/// from `engine_paths.rs`.
fn static_key(key: &KeyPair) -> StaticKey {
    StaticKey::from_stored(&key.private, &key.public).expect("a stored key pair should load")
}

/// The public half, as `ferry-core` names it. Copied from `engine_paths.rs`.
fn public_key(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
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

/// A third pending connection from one source address is refused while the
/// first two are still held, per
/// `ferry_core::limits::MAX_PENDING_HANDSHAKES_PER_ADDR`.
///
/// Before the fix, only the overall `MAX_PENDING_HANDSHAKES` cap of eight
/// existed, so three connections from one address were all accepted and
/// this test's third connection would not have closed at once.
#[test]
fn a_third_pending_connection_from_one_address_is_refused() {
    let target = build("Target");
    target.engine.set_reachable(true);
    let addr = loopback_addr(&target);

    // Neither sends a byte, so each sits waiting on the first-byte deadline,
    // holding its pending slot for up to two seconds: plenty of time to
    // check the third below well inside that window.
    let _first = TcpStream::connect(addr).expect("the first connection is accepted");
    let _second = TcpStream::connect(addr).expect("the second connection is accepted");

    let mut third = TcpStream::connect(addr).expect("the accept itself is never refused");
    third
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("a read timeout can be set");
    let mut buf = [0u8; 1];
    match third.read(&mut buf) {
        Ok(0) => {}
        other => panic!("expected the third connection to be refused at once, got {other:?}"),
    }
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
#[test]
fn a_ninth_serving_connection_from_one_peer_is_refused() {
    let target = build("Target");
    let friend = build("Friend");
    pair_by_code(&target, &friend);

    let addr = loopback_addr(&target);
    let friend_key = static_key(&friend.key);
    let target_public = public_key(&target.key);

    // Each of the first `MAX_SERVING_PER_PEER` connections, reconnecting as
    // the already-paired friend the way `engine_paths.rs`'s hand-built
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
// Helpers used only from here down are added as later findings are fixed.
// ---------------------------------------------------------------------------
