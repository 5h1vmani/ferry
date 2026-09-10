//! The paths the happy path test does not walk.
//!
//! `two_engines.rs` proves that two engines pair and move a file. This file
//! proves what happens when things go wrong: a stop in the middle of a
//! transfer, a peer that is forgotten while it is connected, a first pass
//! that is cut short and picked up again after a restart, a pairing that is
//! picked twice, and a pairing that is confirmed after the watchdog gave up.
//!
//! Each test here is the regression test for one finding of audit 2. The
//! finding is named in the comment above the test.
//!
//! # Why some tests build a peer by hand
//!
//! Some behaviour cannot be shown with two engines, because both engines
//! follow the same rules. A peer that never sends `hello`, a peer that
//! answers one byte at a time, and a stranger that speaks no Ferry at all are
//! built here by hand, out of the same parts the engine uses: `tcp`, `noise`,
//! `rpc` and `frame` from `ferry-core`.
//!
//! # What no test here covers
//!
//! USB is not covered. It needs `adb`, a cable, and a phone. Real mDNS is not
//! covered either. The tests inject an address through `offer_candidate`,
//! because a test machine may sit on a network that refuses multicast. Both
//! are in `docs/manual-checks.md`.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, RpcError, exchange_hello, serve};
use ferry_core::tcp::{self, Listener, Pending};
use ferry_core::version::{MAGIC, VERSION_MAX};
use ferry_runtime::{
    Config, Engine, EngineListener, FerryError, KeyPair, PairingState, TransferState, generate_key,
};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How often a poll looks again.
const POLL_TICK: Duration = Duration::from_millis(10);

/// One mebibyte, which is also the chunk size the engine uses.
const MIB: u64 = 1024 * 1024;

/// That many mebibytes, as a byte count a `Vec` can use.
fn mib(count: u64) -> usize {
    usize::try_from(count * MIB).unwrap_or(usize::MAX)
}

// ---------------------------------------------------------------------------
// The listener the tests give each engine.
// ---------------------------------------------------------------------------

/// What one engine has told the app so far.
#[derive(Default)]
struct Notes {
    /// Every pairing state, in the order it arrived.
    pairings: Vec<PairingState>,
    /// How many times anything changed.
    ticks: u64,
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

    fn tick(&self) {
        self.lock().ticks += 1;
        self.ready.notify_all();
    }

    fn ticks(&self) -> u64 {
        self.lock().ticks
    }

    fn pairing(&self, state: PairingState) {
        let mut notes = self.lock();
        notes.pairings.push(state);
        notes.ticks += 1;
        drop(notes);
        self.ready.notify_all();
    }

    /// Every pairing state seen so far.
    fn states(&self) -> Vec<PairingState> {
        self.lock().pairings.clone()
    }

    /// How many pairing states seen so far match `want`.
    fn count(&self, want: impl Fn(&PairingState) -> bool) -> usize {
        self.lock().pairings.iter().filter(|s| want(s)).count()
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
    fn devices_changed(&self) {
        self.inbox.tick();
    }

    fn transfers_changed(&self) {
        self.inbox.tick();
    }

    fn pairing_changed(&self, state: PairingState) {
        self.inbox.pairing(state);
    }
}

/// Wait until `check` is true, looking again every few milliseconds.
///
/// Some of what these tests watch, such as the byte count of a transfer,
/// moves without a callback, because callbacks are rationed. So this polls
/// instead of waiting on the condition variable.
fn poll_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(POLL_TICK);
    }
    panic!("waited {PATIENCE:?} for {what}");
}

// ---------------------------------------------------------------------------
// Engines under test.
// ---------------------------------------------------------------------------

/// One engine, its inbox, and the folders it owns.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    key: KeyPair,
    data: tempfile::TempDir,
    shared: tempfile::TempDir,
}

impl Side {
    fn shared_root(&self) -> &Path {
        self.shared.path()
    }
}

/// Build an engine on the given folders. It is not started.
fn make_engine(
    name: &str,
    key: KeyPair,
    data: &Path,
    shared: &Path,
    inbox: &Arc<Inbox>,
) -> Result<Arc<Engine>, FerryError> {
    Engine::new(
        Config {
            data_dir: data.to_string_lossy().into_owned(),
            shared_root: shared.to_string_lossy().into_owned(),
            display_name: name.to_owned(),
            listen_port: 0,
            key,
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
    let key = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(name, key.clone(), data.path(), shared.path(), &inbox)
        .expect("the engine should build from a good config");
    engine.start().expect("the engine should start");
    Side {
        engine,
        inbox,
        key,
        data,
        shared,
    }
}

/// The address another engine in this process can dial.
fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
}

fn is_code(state: &PairingState) -> bool {
    matches!(state, PairingState::Code { .. })
}

fn is_confirmed(state: &PairingState) -> bool {
    matches!(state, PairingState::Confirmed { .. })
}

fn is_found(state: &PairingState) -> bool {
    matches!(state, PairingState::Found { .. })
}

fn is_failed(state: &PairingState) -> bool {
    matches!(state, PairingState::Failed { .. })
}

/// The code an error carries, or a panic saying it had none.
fn code_of_error(error: &FerryError) -> String {
    let FerryError::Failed { code, .. } = error;
    code.clone()
}

/// Bytes that are easy to check and hard to get right by accident.
fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap_or(0))
        .collect()
}

// ---------------------------------------------------------------------------
// A peer built by hand.
//
// The engine has no seam for a peer that misbehaves, so these tests speak the
// real protocol: the version exchange, Noise, `hello`, and the file
// operations layer, all from `ferry-core`.
// ---------------------------------------------------------------------------

/// The one file the fake peer serves, and how slowly it answers.
struct Script {
    /// The bytes of `big.bin`.
    bytes: Vec<u8>,
    /// The most bytes one read may answer with. Zero means the whole range.
    read_cap: u32,
    /// Reads at or past this offset wait `slow_pause` before they answer.
    slow_from: u64,
    /// How long a slow read waits.
    slow_pause: Duration,
}

/// A filesystem that serves one file, as slowly as the script says.
struct ScriptedFs {
    script: Mutex<Script>,
    /// How many reads have been answered.
    reads: AtomicU64,
}

impl ScriptedFs {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(Script {
                bytes,
                read_cap: 0,
                slow_from: u64::MAX,
                slow_pause: Duration::ZERO,
            }),
            reads: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Answer at most `cap` bytes per read, so one chunk takes many reads.
    fn cap_reads(&self, cap: u32) {
        self.lock().read_cap = cap;
    }

    /// Wait `pause` on every read at or past `from`.
    fn slow_down(&self, from: u64, pause: Duration) {
        let mut script = self.lock();
        script.slow_from = from;
        script.slow_pause = pause;
    }

    /// Answer every read at once again, in full.
    fn speed_up(&self) {
        let mut script = self.lock();
        script.slow_from = u64::MAX;
        script.slow_pause = Duration::ZERO;
        script.read_cap = 0;
    }

    /// How many reads have been answered so far.
    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl FileOps for ScriptedFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        let script = self.lock();
        Ok(Entry {
            name: "big.bin".to_owned(),
            kind: FileKind::File,
            size: u64::try_from(script.bytes.len()).unwrap_or(0),
            modified_unix_secs: 1_000_000,
        })
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        let (bytes, pause) = {
            let script = self.lock();
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(script.bytes.len());
            let mut want = usize::try_from(length).unwrap_or(0);
            if script.read_cap > 0 {
                want = want.min(usize::try_from(script.read_cap).unwrap_or(want));
            }
            let end = start.saturating_add(want).min(script.bytes.len());
            let pause = if offset >= script.slow_from {
                script.slow_pause
            } else {
                Duration::ZERO
            };
            (script.bytes[start..end].to_vec(), pause)
        };
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(bytes)
    }

    fn write(&self, _path: &RemotePath, _offset: u64, _bytes: &[u8]) -> Result<u32, OpError> {
        Err(OpError::Unsupported)
    }

    fn truncate(&self, _path: &RemotePath, _length: u64) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn rename(&self, _from: &RemotePath, _to: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn set_mtime(&self, _path: &RemotePath, _modified_unix_secs: i64) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn mkdir(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn delete(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }
}

/// A stream the test can break, so a link can fail in the middle.
struct Cutting<S> {
    inner: S,
    cut: Arc<AtomicBool>,
}

impl<S: Read> Read for Cutting<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.cut.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "cut"));
        }
        self.inner.read(out)
    }
}

impl<S: Write> Write for Cutting<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cut.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "cut"));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A peer that listens, pairs once, and then serves what the script says.
struct Peer {
    /// Where the engine dials it.
    addr: SocketAddr,
    /// Its long lived key, as the app would store it.
    key: KeyPair,
    /// What it serves.
    fs: Arc<ScriptedFs>,
    /// True while the next connection is a pairing handshake.
    expect_pair: Arc<AtomicBool>,
    /// How long the peer waits before it sends its name.
    hello_delay: Arc<Mutex<Duration>>,
    /// True while the link is broken.
    cut: Arc<AtomicBool>,
    /// Set to end the accept loop.
    closing: Arc<AtomicBool>,
}

/// Rebuild the Noise key from the bytes the app would have stored.
fn static_key(key: &KeyPair) -> StaticKey {
    StaticKey::from_stored(&key.private, &key.public).expect("a stored key pair should load")
}

/// The public half, as `ferry-core` names it.
fn public_key(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

/// The 64 hex characters an engine knows a device by.
fn key_hex(key: &KeyPair) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for byte in &key.public {
        write!(out, "{byte:02x}").expect("a string always accepts more text");
    }
    out
}

/// Start a peer that answers on its own port.
fn start_peer(engine_key: &KeyPair, bytes: Vec<u8>) -> Peer {
    let listener = Listener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("the fake peer should bind a port");
    let addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        listener.local_addr().port(),
    );
    let key = generate_key().expect("a fresh key pair for the peer");
    let fs = ScriptedFs::new(bytes);
    let peer = Peer {
        addr,
        key: key.clone(),
        fs: Arc::clone(&fs),
        expect_pair: Arc::new(AtomicBool::new(false)),
        hello_delay: Arc::new(Mutex::new(Duration::ZERO)),
        cut: Arc::new(AtomicBool::new(false)),
        closing: Arc::new(AtomicBool::new(false)),
    };

    let engine_public = public_key(engine_key);
    let expect_pair = Arc::clone(&peer.expect_pair);
    let hello_delay = Arc::clone(&peer.hello_delay);
    let cut = Arc::clone(&peer.cut);
    let closing = Arc::clone(&peer.closing);
    drop(std::thread::spawn(move || {
        while !closing.load(Ordering::SeqCst) {
            let Ok(pending) = listener.accept() else {
                continue;
            };
            if closing.load(Ordering::SeqCst) {
                return;
            }
            if cut.load(Ordering::SeqCst) {
                // The link is broken, so nothing is answered on it.
                drop(pending);
                continue;
            }
            let key = static_key(&key);
            let fs = Arc::clone(&fs);
            let cut = Arc::clone(&cut);
            let pairing = expect_pair.swap(false, Ordering::SeqCst);
            let delay = *hello_delay
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(std::thread::spawn(move || {
                serve_one(pending, &key, engine_public, &fs, pairing, delay, &cut);
            }));
        }
    }));
    peer
}

/// Run one connection the peer accepted, to its end.
fn serve_one(
    pending: Pending,
    key: &StaticKey,
    engine: PublicKey,
    fs: &Arc<ScriptedFs>,
    pairing: bool,
    hello_delay: Duration,
    cut: &Arc<AtomicBool>,
) {
    let stream: Box<dyn ReadWrite> = if pairing {
        let Ok(paired) = pending.pair(key) else {
            return;
        };
        Box::new(paired.paired.stream)
    } else {
        let Ok(connection) = pending.connect(key, &engine) else {
            return;
        };
        Box::new(connection.stream)
    };
    let mut stream = Cutting {
        inner: stream,
        cut: Arc::clone(cut),
    };
    std::thread::sleep(hello_delay);
    if exchange_hello(&mut stream, "Fake").is_err() {
        return;
    }
    drop(serve(&mut stream, fs.as_ref()));
}

/// A stream the fake peer can hold whichever handshake produced it.
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

impl Peer {
    /// Break the link: drop what is connected, refuse what arrives.
    fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
    }

    /// Answer again.
    fn mend(&self) {
        self.cut.store(false, Ordering::SeqCst);
    }

    /// Stop the accept loop, which closes the port.
    fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        // One connection, so the blocked accept returns and sees the flag.
        drop(TcpStream::connect_timeout(
            &self.addr,
            Duration::from_secs(2),
        ));
    }
}

/// Pair one engine with a hand-driven peer, and leave the peer able to serve.
fn pair_with_peer(side: &Side, peer: &Peer) {
    side.engine.start_pairing();
    side.engine.offer_candidate(peer.addr);
    side.inbox.wait_pairing("a candidate", is_found);
    peer.expect_pair.store(true, Ordering::SeqCst);
    side.engine
        .pick_candidate(format!("wifi:{}", peer.addr))
        .expect("the injected candidate should be pickable");
    side.inbox.wait_pairing("a code", is_code);
    side.engine.confirm_pairing(true);
    side.inbox.wait_pairing("a confirm", is_confirmed);
}

/// Ask for the one file the fake peer holds.
fn pull_big(side: &Side, peer: &Peer, local_name: &str) -> String {
    side.engine
        .pull(
            key_hex(&peer.key),
            "big.bin".to_owned(),
            local_name.to_owned(),
        )
        .expect("the pull should be accepted")
}

/// Wait until one transfer satisfies `check`.
fn wait_transfer(
    side: &Side,
    id: &str,
    what: &str,
    check: impl Fn(&ferry_runtime::TransferInfo) -> bool,
) {
    let engine = Arc::clone(&side.engine);
    let wanted = id.to_owned();
    poll_until(what, move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted && check(t))
    });
}

// ---------------------------------------------------------------------------
// Finding 1: stop during a transfer.
// ---------------------------------------------------------------------------

#[test]
fn stop_returns_while_a_transfer_is_moving() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(8)));
    // Small answers, each after a wait, so the transfer is still running
    // when `stop` is called.
    peer.fs.cap_reads(64 * 1024);
    peer.fs.slow_down(0, Duration::from_millis(20));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the transfer to start moving", |t| {
        t.bytes_done > 0
    });

    let started = Instant::now();
    side.engine.stop();
    let took = started.elapsed();
    peer.close();
    assert!(took < Duration::from_secs(3), "stop took {took:?}");
}

// ---------------------------------------------------------------------------
// Finding 2: stop while one side has confirmed and the other has not.
// ---------------------------------------------------------------------------

#[test]
fn stop_returns_when_only_one_side_confirmed() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    phone.engine.set_reachable(true);
    phone.engine.start_pairing();
    mac.engine.start_pairing();
    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    mac.engine
        .pick_candidate(format!("wifi:{phone_addr}"))
        .expect("the injected candidate should be pickable");
    mac.inbox.wait_pairing("the Mac's code", is_code);
    phone.inbox.wait_pairing("the phone's code", is_code);

    // Only the Mac confirms. The phone sends no name, so the Mac's name
    // exchange waits, and `stop` used to wait with it.
    mac.engine.confirm_pairing(true);

    let started = Instant::now();
    mac.engine.stop();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "stop took {took:?}");
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Finding 3: forget must reach a connection whose hello has not arrived.
// ---------------------------------------------------------------------------

#[test]
fn forget_refuses_a_peer_that_has_not_said_hello() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    // The peer dials in and finishes the handshake, then says nothing.
    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;

    // Let the engine's serving thread reach its wait for the name.
    std::thread::sleep(Duration::from_millis(300));

    phone
        .engine
        .forget(key_hex(&peer.key))
        .expect("a paired device can be forgotten");

    // Now the peer speaks. The engine answers the hello, because that
    // exchange was already waiting, and then finds the device is no longer
    // listed and closes the connection. The first operation fails on the
    // closed connection, however late the hello is.
    exchange_hello(&mut stream, "Fake").expect("the name exchange still runs");
    let mut client = Client::new(stream);
    let asked = client.read(
        &RemotePath::parse("anything.bin").expect("a valid path"),
        0,
        16,
    );
    match asked {
        Err(RpcError::Frame(_)) => {}
        other => panic!("the connection should be closed, got {other:?}"),
    }
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Finding 4: stop must stop serving files.
// ---------------------------------------------------------------------------

#[test]
fn stop_stops_serving_a_connected_peer() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Fake").expect("the name exchange runs");
    let mut client = Client::new(stream);
    let path = RemotePath::parse("note.txt").expect("a valid path");
    let first = client.read(&path, 0, 5).expect("a paired peer may read");
    assert_eq!(first, b"hello", "the file is served while the engine runs");

    phone.engine.stop();

    match client.read(&path, 0, 5) {
        Err(RpcError::Remote(OpError::PermissionDenied)) => {}
        other => panic!("the read after stop should be refused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Finding 5: an interrupted first pass survives a restart.
// ---------------------------------------------------------------------------

#[test]
fn an_interrupted_first_pass_resumes_after_a_restart() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(8)));
    // The first two chunks arrive at once. Everything after them crawls, so
    // the test can stop the engine inside the third chunk.
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(2 * MIB, Duration::from_millis(500));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "two chunks to arrive", |t| {
        t.bytes_done >= 2 * MIB
    });
    side.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    peer.fs.speed_up();
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let found = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the interrupted transfer should be listed again");
    assert_eq!(
        found.state,
        TransferState::Paused,
        "an interrupted transfer comes back paused"
    );
    assert_eq!(
        found.bytes_done,
        2 * MIB,
        "it comes back at the last verified chunk"
    );
    assert_eq!(found.bytes_total, 8 * MIB, "it knows the size of the file");

    // The peer is reachable again, so the transfer finishes on its own.
    engine.offer_candidate(peer.addr);
    engine.start().expect("the engine should start");
    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("the transfer to finish", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });
    let landed = std::fs::read(side.shared_root().join("big.bin")).expect("the file should land");
    assert_eq!(landed, sample_bytes(mib(8)), "every byte must match");

    engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// Finding 6: one engine per data folder, and no record for a device that is
// not paired.
// ---------------------------------------------------------------------------

#[test]
fn a_second_engine_on_one_data_folder_is_refused() {
    let side = build("Vamana");
    let inbox = Arc::new(Inbox::default());
    let second = make_engine(
        "Vamana again",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        &inbox,
    );
    let error = second.err().expect("a second engine must be refused");
    assert_eq!(code_of_error(&error), "Runtime::BadConfig");
    side.engine.stop();

    // Once the first engine has stopped, the folder is free again.
    let third = make_engine(
        "Vamana later",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        &inbox,
    )
    .expect("the folder is free once the first engine stopped");
    third.stop();
}

#[test]
fn a_record_for_a_device_that_is_not_paired_is_dropped() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(4)));
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(MIB, Duration::from_millis(500));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "one chunk to arrive", |t| t.bytes_done >= MIB);
    side.engine.stop();
    peer.close();

    // The device list is lost, the record is not. This is the shape a second
    // engine on one folder used to leave behind.
    std::fs::remove_file(side.data.path().join("peers.bin")).expect("the list should be there");

    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        &inbox,
    )
    .expect("the engine should build");
    engine.start().expect("the engine should start");
    assert!(
        engine.transfers().is_empty(),
        "a record whose device is not paired must be dropped"
    );
    let left: Vec<String> = std::fs::read_dir(side.data.path().join("transfers"))
        .expect("the transfers folder should be readable")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.is_empty(), "its record file goes too, found {left:?}");
    engine.stop();
}

// ---------------------------------------------------------------------------
// Finding 7: no callback after stop returned.
// ---------------------------------------------------------------------------

#[test]
fn no_callback_arrives_after_stop_returned() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Fake").expect("the name exchange runs");

    let Side {
        engine,
        inbox,
        key: _key,
        data: _data,
        shared: _shared,
    } = phone;
    engine.stop();
    drop(engine);
    let quiet_at = inbox.ticks();

    // The connection ends now. The serving thread wakes, finishes, and used
    // to report a device change into an engine that no longer exists.
    drop(stream);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        inbox.ticks(),
        quiet_at,
        "no callback may arrive after stop returned"
    );
}

// ---------------------------------------------------------------------------
// Finding 8: the candidate list is capped and its reports are rationed.
// ---------------------------------------------------------------------------

#[test]
fn the_candidate_list_is_capped_and_its_reports_are_rationed() {
    let mac = build("Vamana");
    mac.engine.set_pairing_timeout(Duration::from_secs(30));
    mac.engine.start_pairing();

    let started = Instant::now();
    for port in 20_000u16..20_200 {
        mac.engine
            .offer_candidate(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    let injected = started.elapsed();

    mac.inbox.wait_pairing("a candidate", is_found);
    // Long enough for a rationed report to arrive after the last candidate.
    std::thread::sleep(Duration::from_millis(1500));

    let founds = mac.inbox.count(is_found);
    assert!(
        founds <= 5,
        "200 candidates in {injected:?} produced {founds} reports"
    );
    let held = mac
        .inbox
        .states()
        .into_iter()
        .filter_map(|state| match state {
            PairingState::Found { candidates } => Some(candidates.len()),
            _ => None,
        })
        .max()
        .expect("at least one report of candidates");
    assert!(held <= 32, "the list must be capped, it held {held}");
    mac.engine.stop();
}

// ---------------------------------------------------------------------------
// Finding 9: one code at a time, and no confirm after the watchdog.
// ---------------------------------------------------------------------------

#[test]
fn picking_a_candidate_twice_is_refused_and_shows_one_code() {
    let mac = build("Vamana");
    let peer = start_peer(&mac.key, sample_bytes(16));
    mac.engine.set_pairing_timeout(Duration::from_secs(30));
    mac.engine.start_pairing();
    mac.engine.offer_candidate(peer.addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    peer.expect_pair.store(true, Ordering::SeqCst);

    let id = format!("wifi:{}", peer.addr);
    mac.engine
        .pick_candidate(id.clone())
        .expect("the first pick starts a handshake");
    let second = mac
        .engine
        .pick_candidate(id)
        .expect_err("the second pick must be refused");
    assert_eq!(code_of_error(&second), "Runtime::PairingBusy");

    mac.inbox.wait_pairing("a code", is_code);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        mac.inbox.count(is_code),
        1,
        "only one code may ever be shown"
    );
    mac.engine.stop();
    peer.close();
}

#[test]
fn a_confirm_after_the_watchdog_stores_nothing() {
    let mac = build("Vamana");
    let peer = start_peer(&mac.key, sample_bytes(16));
    // The peer holds its name back, so the confirm is still running when
    // pairing times out.
    *peer
        .hello_delay
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Duration::from_millis(1500);

    mac.engine.set_pairing_timeout(Duration::from_millis(700));
    mac.engine.start_pairing();
    mac.engine.offer_candidate(peer.addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    peer.expect_pair.store(true, Ordering::SeqCst);
    mac.engine
        .pick_candidate(format!("wifi:{}", peer.addr))
        .expect("the candidate should be pickable");
    mac.inbox.wait_pairing("a code", is_code);
    mac.engine.confirm_pairing(true);

    mac.inbox.wait_pairing("the watchdog to give up", is_failed);
    // Long enough for the late name to arrive and be ignored.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        mac.inbox.count(is_confirmed),
        0,
        "a pairing that failed must never confirm"
    );
    assert!(
        mac.engine.devices().is_empty(),
        "a pairing that failed must store no device"
    );
    mac.engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// Finding 10: a few threads, and callbacks that are rationed.
// ---------------------------------------------------------------------------

#[test]
fn many_pulls_share_a_few_threads_and_few_callbacks() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(16));
    pair_with_peer(&side, &peer);
    // Nothing answers from here on, so every attempt fails and retries.
    peer.close();

    let ticks_before = side.inbox.ticks();
    let started = Instant::now();
    for i in 0..200 {
        drop(pull_big(&side, &peer, &format!("copy-{i}.bin")));
    }
    let ticks = side.inbox.ticks() - ticks_before;
    let took = started.elapsed();
    assert!(
        ticks < 20,
        "200 pulls in {took:?} produced {ticks} callbacks"
    );
    assert_eq!(side.engine.transfers().len(), 200, "all of them are listed");
    let workers = side.engine.transfer_workers();
    assert!(
        workers <= 4,
        "200 pulls must not become 200 threads, {workers} workers ran"
    );
    side.engine.stop();
}

// ---------------------------------------------------------------------------
// Finding 11: a peer that answers one byte at a time.
// ---------------------------------------------------------------------------

#[test]
fn a_peer_that_answers_one_byte_at_a_time_does_not_hold_the_transfer() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(4)));
    peer.fs.cap_reads(1);
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    let engine = Arc::clone(&side.engine);
    let wanted = id.clone();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut paused = false;
    while Instant::now() < deadline {
        if engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Paused)
        {
            paused = true;
            break;
        }
        std::thread::sleep(POLL_TICK);
    }
    assert!(
        paused,
        "the transfer must leave Active, after {} reads",
        peer.fs.reads()
    );
    side.engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// Finding 14: a stranger learns nothing from the first bytes.
// ---------------------------------------------------------------------------

/// Connect, speak the version exchange, and return what came back.
fn first_bytes_from(addr: SocketAddr) -> Vec<u8> {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("the port should answer");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout should be accepted");
    let mut hello = Vec::with_capacity(7);
    hello.extend_from_slice(&MAGIC);
    hello.extend_from_slice(&VERSION_MAX.to_be_bytes());
    // A peer that is not welcome may close before this is written.
    drop(stream.write_all(&hello));
    drop(stream.flush());
    let mut answer = [0u8; 7];
    let mut got = 0usize;
    while got < answer.len() {
        match stream.read(&mut answer[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    answer[..got].to_vec()
}

#[test]
fn a_stranger_cannot_tell_whether_the_device_has_a_peer() {
    let mut expected = Vec::with_capacity(7);
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&VERSION_MAX.to_be_bytes());

    let alone = build("Pixel 3 XL");
    alone.engine.set_reachable(true);
    let answer_alone = first_bytes_from(loopback_addr(&alone));

    let paired = build("Pixel 3 XL");
    paired.engine.set_reachable(true);
    let peer = start_peer(&paired.key, sample_bytes(16));
    pair_with_peer(&paired, &peer);
    peer.close();
    let answer_paired = first_bytes_from(loopback_addr(&paired));

    assert_eq!(
        answer_alone, expected,
        "a device with no peer answers the version exchange"
    );
    assert_eq!(
        answer_paired, expected,
        "a device with one peer answers the same bytes"
    );
    alone.engine.stop();
    paired.engine.stop();
}

// ---------------------------------------------------------------------------
// The acceptance paths: a broken link, and a device that is not reachable.
// ---------------------------------------------------------------------------

#[test]
fn a_transfer_pauses_when_the_link_breaks_and_finishes_when_it_returns() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(4)));
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(MIB, Duration::from_millis(300));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the first chunk to arrive", |t| {
        t.bytes_done >= MIB
    });

    // Break the link, the way a phone leaving the network breaks it.
    peer.cut();
    wait_transfer(&side, &id, "the transfer to pause", |t| {
        t.state == TransferState::Paused
    });

    // Bring it back, and let the retry loop find it.
    peer.fs.speed_up();
    peer.mend();
    wait_transfer(&side, &id, "the transfer to finish", |t| {
        t.state == TransferState::Done
    });
    let landed = std::fs::read(side.shared_root().join("big.bin")).expect("the file should land");
    assert_eq!(landed, sample_bytes(mib(4)), "every byte must match");
    side.engine.stop();
    peer.close();
}

#[test]
fn a_device_that_is_not_reachable_refuses_a_new_connection() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    phone.engine.set_reachable(false);
    let refused = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    );
    assert!(
        refused.is_err(),
        "a device that is not reachable accepts nothing"
    );

    phone.engine.set_reachable(true);
    let accepted = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    );
    assert!(accepted.is_ok(), "it accepts again once it is reachable");
    phone.engine.stop();
}
