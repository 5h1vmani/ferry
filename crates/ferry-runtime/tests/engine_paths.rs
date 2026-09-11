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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_core::chunk::{ChunkSize, Manifest, manifest_from_bytes};
use ferry_core::memfs::MemoryFs;
use ferry_core::noise::{PublicKey, SecureStream, StaticKey};
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind;
use ferry_core::rpc::{Client, FileOps, RpcError, exchange_hello, serve};
use ferry_core::session::Transfer;
use ferry_core::tcp::{self, Listener, Pending};
use ferry_core::version::{MAGIC, VERSION_MAX};
use ferry_runtime::{
    AccessVerb, Actor, Config, DeviceKind as RuntimeDeviceKind, Direction, Engine, EngineListener,
    FerryError, KeyPair, PairingMethod, PairingState, Root, TransferState, generate_key,
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

    /// Forget every pairing state seen so far.
    ///
    /// `wait_pairing` finds the first recorded state `want` accepts, and
    /// never forgets one on its own. Pairing a second peer with the same
    /// engine needs this first, or `wait_pairing` matches the first
    /// pairing's own `Found`, `Code`, or `Confirmed` state instead of
    /// waiting for the second one's.
    fn clear_pairings(&self) {
        self.lock().pairings.clear();
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

    fn access_log_changed(&self) {
        self.inbox.tick();
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
    download: tempfile::TempDir,
}

impl Side {
    /// The one root this side serves, named `"Root"`.
    fn shared_root(&self) -> &Path {
        self.shared.path()
    }

    /// Where a pull lands. Never the same folder as `shared_root`.
    fn download_root(&self) -> &Path {
        self.download.path()
    }
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
        data,
        shared,
        download,
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

    fn manifest(&self, path: &RemotePath) -> Result<Manifest, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        // The same chunk size the engine defaults to, so a first pass's own
        // fetch loop, driven by this manifest, asks for exactly the ranges
        // every other test here already expects.
        let bytes = self.lock().bytes.clone();
        Ok(manifest_from_bytes(&bytes, ChunkSize::one_mebibyte()))
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
///
/// Generic over what it serves: most tests use [`ScriptedFs`], the one file
/// and slow-reading script above; a few build a small `FileOps` of their
/// own, to prove what happens when a peer's `list` misbehaves.
struct Peer<F> {
    /// Where the engine dials it.
    addr: SocketAddr,
    /// Its long lived key, as the app would store it.
    key: KeyPair,
    /// What it serves.
    fs: Arc<F>,
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

/// Start a peer that serves one file, as slowly as the script says.
fn start_peer(engine_key: &KeyPair, bytes: Vec<u8>) -> Peer<ScriptedFs> {
    start_peer_with(engine_key, ScriptedFs::new(bytes))
}

/// Start a peer that answers on its own port, serving whatever `fs` says.
fn start_peer_with<F: FileOps + Send + Sync + 'static>(
    engine_key: &KeyPair,
    fs: Arc<F>,
) -> Peer<F> {
    let listener = Listener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("the fake peer should bind a port");
    let addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        listener.local_addr().port(),
    );
    let key = generate_key().expect("a fresh key pair for the peer");
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
fn serve_one<F: FileOps + Send + Sync>(
    pending: Pending,
    key: &StaticKey,
    engine: PublicKey,
    fs: &Arc<F>,
    pairing: bool,
    hello_delay: Duration,
    cut: &Arc<AtomicBool>,
) {
    let Ok(negotiated) = pending.negotiate() else {
        return;
    };
    let stream: Box<dyn ReadWrite> = if pairing {
        let Ok(paired) = negotiated.pair(key) else {
            return;
        };
        Box::new(paired.paired.stream)
    } else {
        let Ok(connection) = negotiated.connect(key, &[engine]) else {
            return;
        };
        Box::new(connection.stream)
    };
    let mut stream = Cutting {
        inner: stream,
        cut: Arc::clone(cut),
    };
    std::thread::sleep(hello_delay);
    if exchange_hello(&mut stream, "Fake", DeviceKind::Phone).is_err() {
        return;
    }
    drop(serve(&mut stream, fs.as_ref()));
}

/// A stream the fake peer can hold whichever handshake produced it.
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

impl<F> Peer<F> {
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
fn pair_with_peer<F>(side: &Side, peer: &Peer<F>) {
    side.engine.start_pairing_with(PairingMethod::Code);
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
fn pull_big<F>(side: &Side, peer: &Peer<F>, local_name: &str) -> String {
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
// Batch D audit, B1: a folder listing that never advances.
// ---------------------------------------------------------------------------

/// A filesystem whose `list` never lets a folder listing finish: every call
/// answers the same one entry and the same cursor, whatever cursor was
/// asked for. Stands in for a peer that never advances its own paging.
struct StuckListFs;

impl FileOps for StuckListFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: "a.jpg".to_owned(),
                kind: FileKind::File,
                size: 1,
                modified_unix_secs: 1_000_000,
            }],
            Some(0),
        ))
    }
    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }
    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
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
    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Err(OpError::Unsupported)
    }
}

#[test]
fn a_peer_whose_cursor_never_advances_makes_list_and_pull_folder_refuse_it() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(StuckListFs));
    pair_with_peer(&side, &peer);

    let list_error = side
        .engine
        .list(key_hex(&peer.key), "Camera".to_owned())
        .expect_err("a peer that never advances its cursor should be refused");
    assert_eq!(code_of_error(&list_error), "Runtime::FolderTooLarge");

    let folder_error = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect_err("pull_folder pages the same way and should be refused too");
    assert_eq!(code_of_error(&folder_error), "Runtime::FolderTooLarge");
    assert!(
        side.engine.batches().is_empty(),
        "a refused folder listing creates no batch"
    );
    assert!(
        side.engine.transfers().is_empty(),
        "a refused folder listing creates no transfer"
    );

    peer.close();
}

// ---------------------------------------------------------------------------
// Batch D audit, B2: an entry that names no real child.
// ---------------------------------------------------------------------------

/// A filesystem whose one `list` entry has an empty name.
struct EmptyNameFs;

impl FileOps for EmptyNameFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: String::new(),
                kind: FileKind::File,
                size: 1,
                modified_unix_secs: 1_000_000,
            }],
            None,
        ))
    }
    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }
    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
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
    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Err(OpError::Unsupported)
    }
}

#[test]
fn pull_folder_fails_cleanly_when_a_peer_names_an_entry_with_an_empty_name() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(EmptyNameFs));
    pair_with_peer(&side, &peer);

    let error = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect_err("an empty entry name should be refused on decode, not queued");
    assert_eq!(code_of_error(&error), "WireError::InvalidPath");
    assert!(
        side.engine.batches().is_empty(),
        "a refused folder listing creates no batch"
    );
    assert!(
        side.engine.transfers().is_empty(),
        "a refused folder listing creates no transfer"
    );

    peer.close();
}

// ---------------------------------------------------------------------------
// Batch B and C audit, C4: a download folder set before start.
// ---------------------------------------------------------------------------

#[test]
fn set_download_dir_before_start_is_where_the_next_pull_lands() {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared_root = tempfile::tempdir().expect("a temporary folder for shared files");
    // Passed to `Config`, but never used: `set_download_dir` below opens a
    // fresher folder before `start` gets the chance to open this one.
    let configured_download = tempfile::tempdir().expect("a temporary folder for the config");
    let real_download = tempfile::tempdir().expect("the folder set_download_dir should open");
    let key = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        key.clone(),
        data.path(),
        shared_root.path(),
        configured_download.path(),
        &inbox,
    )
    .expect("the engine should build from a good config");

    engine
        .set_download_dir(real_download.path().to_string_lossy().into_owned())
        .expect("set_download_dir should accept a good folder before start");
    engine.start().expect("the engine should start");

    let peer = start_peer(&key, sample_bytes(16));
    let side = Side {
        engine,
        inbox,
        key,
        data,
        shared: shared_root,
        download: configured_download,
    };
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the pull to finish", |t| {
        t.state == TransferState::Done
    });

    assert!(
        real_download.path().join("big.bin").exists(),
        "the pull lands in the folder set_download_dir opened before start"
    );
    assert!(
        !side.download_root().join("big.bin").exists(),
        "not in the folder Config named, which start would otherwise have opened"
    );

    side.engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// Batch B and C audit, C6: a retried transfer clears its end time.
// ---------------------------------------------------------------------------

#[test]
fn retry_clears_the_transfers_end_time() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(16));
    pair_with_peer(&side, &peer);

    // `ScriptedFs::stat` answers `NotFound` for anything but "big.bin", so
    // this fails at once, the peer having refused the very first call.
    let id = side
        .engine
        .pull(
            key_hex(&peer.key),
            "missing.bin".to_owned(),
            "missing.bin".to_owned(),
        )
        .expect("the pull is accepted; the peer refuses it once dialled");
    wait_transfer(&side, &id, "the pull to fail", |t| {
        t.state == TransferState::Failed
    });
    let failed = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is listed");
    assert!(
        failed.ended_unix_secs.is_some(),
        "a failed transfer has an end time"
    );

    side.engine
        .retry(id.clone())
        .expect("a failed transfer can be retried");
    // Caught the moment retry has cleared the row and requeued it, before
    // the same refusal fails it again.
    wait_transfer(&side, &id, "retry to clear the end time", |t| {
        t.state == TransferState::Queued && t.ended_unix_secs.is_none()
    });

    side.engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// Batch B and C audit, C6: a version 1 peer file assumes this device's
// opposite kind.
// ---------------------------------------------------------------------------

#[test]
fn a_phone_loading_a_version_1_peer_file_assumes_each_peer_is_a_mac() {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared_root = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key = generate_key().expect("a fresh key pair");
    let peer_key = generate_key().expect("a fresh key pair for the stored peer");

    // A version 1 peer file, written by hand in the format `PeerStore`
    // wrote before item 11 added a kind byte per peer: version, count, then
    // each peer's key, name, and paired time, with no kind byte at all.
    let mut e = ferry_core::wire::Encoder::new();
    e.u8(1);
    e.u32(1);
    e.fixed(public_key(&peer_key).as_bytes());
    e.text("An old Mac");
    e.u64(u64::from_ne_bytes(1_700_000_000i64.to_ne_bytes()));
    std::fs::write(data.path().join("peers.bin"), e.finish())
        .expect("the hand-built version 1 peer file should write");

    let inbox = Arc::new(Inbox::default());
    let engine = Engine::new(
        Config {
            data_dir: data.path().to_string_lossy().into_owned(),
            shared_roots: vec![Root {
                name: "Root".to_owned(),
                path: shared_root.path().to_string_lossy().into_owned(),
                writable: true,
            }],
            download_dir: download.path().to_string_lossy().into_owned(),
            display_name: "Pixel 3 XL".to_owned(),
            listen_port: 0,
            key,
            kind: RuntimeDeviceKind::Phone,
        },
        Box::new(Recorder {
            inbox: Arc::clone(&inbox),
        }),
    )
    .expect("a version 1 peer file should still load");

    let devices = engine.devices();
    assert_eq!(devices.len(), 1, "the stored peer should load");
    assert_eq!(
        devices[0].kind,
        RuntimeDeviceKind::Mac,
        "a version 1 file predates the kind byte; a phone assumes every peer in it is a Mac"
    );
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
    // Item 8: speed_bytes_per_sec is measured over the last two seconds of
    // this transfer's own bytes, so it takes a moment to appear.
    wait_transfer(&side, &id, "a per-transfer speed to appear", |t| {
        t.speed_bytes_per_sec.is_some()
    });
    let moving = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the transfer should be listed while it moves");
    assert_eq!(
        moving.state,
        TransferState::Active,
        "a speed is reported only while the state is Active"
    );
    assert!(
        moving.speed_bytes_per_sec.expect("checked above") > 0,
        "a transfer moving real bytes has a nonzero speed"
    );

    let started = Instant::now();
    side.engine.stop();
    let took = started.elapsed();

    let stopped = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the transfer is still listed once stop returns");
    assert_eq!(
        stopped.speed_bytes_per_sec, None,
        "speed_bytes_per_sec is None once the state is no longer Active"
    );

    peer.close();
    assert!(took < Duration::from_secs(3), "stop took {took:?}");
}

// ---------------------------------------------------------------------------
// docs/engine-contract.md item 16c: stop closes every socket, so a worker
// blocked in a kernel read cannot hold it up.
// ---------------------------------------------------------------------------

/// A filesystem that accepts the handshake and answers `stat` and
/// `manifest` honestly, but never answers a `read`. Stands in for a peer
/// that accepts a connection and then goes silent mid-transfer.
struct NeverAnswersFs {
    bytes: Vec<u8>,
}

impl FileOps for NeverAnswersFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        Ok(Entry {
            name: "big.bin".to_owned(),
            kind: FileKind::File,
            size: u64::try_from(self.bytes.len()).unwrap_or(0),
            modified_unix_secs: 1_000_000,
        })
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        // Never answers. The serving thread parks here for the rest of the
        // test process's life; nothing needs it to return, since what this
        // test checks is how quickly the engine's own side gives up.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
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

    fn manifest(&self, path: &RemotePath) -> Result<Manifest, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        Ok(manifest_from_bytes(&self.bytes, ChunkSize::one_mebibyte()))
    }
}

#[test]
fn stop_returns_quickly_when_a_peer_accepts_and_never_answers_a_read() {
    let side = build("Vamana");
    let peer = start_peer_with(
        &side.key,
        Arc::new(NeverAnswersFs {
            bytes: sample_bytes(mib(1)),
        }),
    );
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "hangs.bin");
    wait_transfer(&side, &id, "the pull to start", |t| {
        t.state == TransferState::Active
    });

    let started = Instant::now();
    side.engine.stop();
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(2),
        "stop took {took:?}, expected under two seconds"
    );

    peer.close();
}

// ---------------------------------------------------------------------------
// Finding 2: stop while one side has confirmed and the other has not.
// ---------------------------------------------------------------------------

#[test]
fn stop_returns_when_only_one_side_confirmed() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    phone.engine.set_reachable(true);
    phone.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.start_pairing_with(PairingMethod::Code);
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
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange still runs");
    let mut client = Client::new(stream);
    let asked = client.read(
        &RemotePath::parse("Root/anything.bin").expect("a valid path"),
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
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange runs");
    let mut client = Client::new(stream);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    let first = client.read(&path, 0, 5).expect("a paired peer may read");
    assert_eq!(first, b"hello", "the file is served while the engine runs");

    phone.engine.stop();

    // docs/engine-contract.md item 16c: `stop` now closes every registered
    // socket directly, so a call already in flight on this connection meets
    // a closed socket, not the clean `PermissionDenied` `GuardedFs`'s switch
    // used to answer with while the socket stayed open.
    assert!(
        client.read(&path, 0, 5).is_err(),
        "the connection should be gone once stop has closed its socket"
    );
}

// ---------------------------------------------------------------------------
// Finding 5: an interrupted first pass survives a restart.
// ---------------------------------------------------------------------------

#[test]
// One long, linear narrative is the point of this test: a transfer cut
// short, restarted, and checked at each stage. Splitting it into smaller
// functions would hide that it is one path, not several.
#[allow(clippy::too_many_lines)]
fn an_interrupted_first_pass_resumes_after_a_restart() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(mib(8)));
    // The first two chunks arrive at once. Everything after them crawls, so
    // the test can stop the engine inside the third chunk. The same crawl
    // continues through the resume below: four reads at this cap, each
    // paused this long, take about three seconds for the first resumed
    // chunk, comfortably past the two second window item 8's speed is
    // measured over.
    peer.fs.cap_reads(256 * 1024);
    peer.fs.slow_down(2 * MIB, Duration::from_millis(750));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "two chunks to arrive", |t| {
        t.bytes_done >= 2 * MIB
    });
    let before_stop = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the transfer should be listed before stop");
    assert_eq!(
        before_stop.direction,
        Direction::Pull,
        "a pull is always Direction::Pull until item 5"
    );
    assert_eq!(
        before_stop.ended_unix_secs, None,
        "a transfer still moving has no end time"
    );
    side.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    // The link stays slow a little longer: item 8's speed is checked below,
    // right after the resume starts, while it is still measurable.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
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
    assert_eq!(
        found.started_unix_secs, before_stop.started_unix_secs,
        "the start time survives the restart, item 9's whole point"
    );
    assert_eq!(
        found.ended_unix_secs, None,
        "a record this crate writes never carries an end time"
    );
    assert_eq!(
        found.direction,
        Direction::Pull,
        "a restarted transfer is still a pull"
    );
    // Item 7: 8 MiB at a 1 MiB chunk size is 8 chunks, and 2 MiB verified in
    // place is exactly 2 whole chunks, so both numbers land on an exact
    // count rather than needing to round.
    assert_eq!(found.chunks_total, 8, "8 MiB at 1 MiB chunks is 8 chunks");
    assert_eq!(
        found.chunks_verified, 2,
        "2 MiB verified in place is 2 whole chunks"
    );

    // The peer is reachable again, so the transfer finishes on its own.
    engine.offer_candidate(peer.addr);
    engine.start().expect("the engine should start");

    // C2: the resume starts from 2 MiB already verified. The link is still
    // the slow one set up above, so the first newly moved chunk takes about
    // three seconds, long enough for a speed to be reported. Before the fix
    // the window started at zero bytes, so this first speed would count the
    // 2 MiB baseline as if it had just moved, several times faster than the
    // link this test allows. It is checked here, before `speed_up` below
    // lets the rest of the file arrive at once.
    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("a speed for the resumed transfer to appear", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.speed_bytes_per_sec.is_some())
    });
    let resumed_speed = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .and_then(|t| t.speed_bytes_per_sec)
        .expect("checked by the poll above");
    assert!(
        resumed_speed < 500_000,
        "the resumed transfer's first speed should reflect only new bytes, not the \
         2 MiB baseline too; got {resumed_speed} bytes/sec"
    );

    peer.fs.speed_up();
    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("the transfer to finish", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });
    let landed = std::fs::read(side.download_root().join("big.bin")).expect("the file should land");
    assert_eq!(landed, sample_bytes(mib(8)), "every byte must match");

    let done = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the finished transfer is still listed");
    assert_eq!(
        done.started_unix_secs, before_stop.started_unix_secs,
        "the start time is the same one throughout the transfer's life"
    );
    let ended = done
        .ended_unix_secs
        .expect("a done transfer has an end time");
    assert!(
        ended >= done.started_unix_secs,
        "the end time is not before the start time"
    );
    assert_eq!(
        done.chunks_verified, done.chunks_total,
        "every chunk is verified once the transfer is Done"
    );

    engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// G10: a Record::Ready row whose landing file is missing starts over.
// ---------------------------------------------------------------------------

/// The bytes of a `Record::Ready` row, in the exact shape `record.rs`
/// writes: format version 3, the ready stage byte, the transfer, then its
/// meta fields. Built by hand because `record.rs`'s own types are private
/// to `ferry-runtime`; this is the same technique other tests here use for
/// a hand-built peer file.
fn encode_ready_record(transfer: &Transfer) -> Vec<u8> {
    let mut e = ferry_core::wire::Encoder::new();
    e.u8(3); // record.rs FORMAT_VERSION
    e.u8(1); // record.rs STAGE_READY
    e.bytes(&transfer.encode());
    // Meta: started_unix_secs, ended_unix_secs (None), direction (Pull),
    // batch_id (None).
    e.fixed(&1_700_000_000i64.to_be_bytes());
    e.u8(0);
    e.u8(0);
    e.u8(0);
    e.finish()
}

/// G10: a `Record::Ready` row's fixed temporary name lives under its
/// destination's own folder. `verify_and_land` used to trust that folder
/// was still the one an earlier first pass made it in. If the download
/// folder changed since -- or that folder was simply removed by hand --
/// the next attempt found no parent to write into and failed for good,
/// rather than recreating it and fetching the file again from nothing:
/// exactly what "the manifest is a hint, the disk is the truth"
/// (docs/protocol.md section 9) already means for a file that is not
/// merely short, but missing outright.
///
/// The row here is built by hand rather than produced by a real
/// interrupted transfer, because nothing stops between the moment a real
/// first pass finishes and the same attempt landing the file: both run on
/// the same connection, one call apart. A stored record with a manifest
/// but no fixed temporary name behind it is exactly what an app killed in
/// that narrow window would leave, so this is that state, not a
/// contrivance.
#[test]
fn a_ready_record_whose_landing_folder_is_gone_lands_in_the_current_one() {
    let side = build("Vamana");
    let bytes = sample_bytes(mib(2));
    let peer = start_peer(&side.key, bytes.clone());
    pair_with_peer(&side, &peer);
    let device = key_hex(&peer.key);
    side.engine.stop();

    // A nested destination, not a flat one, so the missing "Camera" folder
    // itself -- not just a missing file at the download root -- is what
    // the fix must recreate.
    let manifest = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    let source = RemotePath::parse("big.bin").expect("a valid path");
    let destination = RemotePath::parse("Camera/big.bin").expect("a valid path");
    let transfer =
        Transfer::new(manifest, source, destination).expect("a fresh transfer should build");
    let id = format!("{device}-g10manual");
    std::fs::write(
        side.data.path().join("transfers").join(format!("{id}.bin")),
        encode_ready_record(&transfer),
    )
    .expect("the hand-built record should write");

    // A fresh engine on the same data and shared folders, but a brand new
    // download folder: "the download folder changed between two
    // attempts".
    let new_download = tempfile::tempdir().expect("a new download folder");
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        new_download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let found = engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the hand-built record should be listed");
    assert_eq!(
        found.state,
        TransferState::Paused,
        "a loaded record comes back paused"
    );

    engine.offer_candidate(peer.addr);
    engine.start().expect("the engine should start");

    let watching = Arc::clone(&engine);
    let wanted = id.clone();
    poll_until("the transfer to land from nothing", move || {
        watching
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });

    assert_eq!(
        std::fs::read(new_download.path().join("Camera/big.bin"))
            .expect("the file should land in the new download folder"),
        bytes,
        "every byte must match, fetched fresh since nothing was on disk"
    );

    engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// Item 5: a push resumes across a cut connection.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md` item 5: a push cut at three points -- early,
/// mid-transfer, and during the final rename -- resumes on its own and
/// rewrites at most one chunk. Proved with wire bytes, the technique
/// `resume_sweep.rs` uses for a pull, but at three points rather than a
/// full sweep: item 5 says the resume rule is the same code path that sweep
/// already proves, so this only has to show a push reaches it too.
///
/// Two real engines, the way `pair_two_engines` sets up for a batch: a
/// pull's own hand-built `Peer` only answers `read`, and a push needs
/// `write`, `truncate`, `rename`, `set_mtime` and the manifest request
/// served for real.
#[test]
fn a_cut_push_resumes_and_rewrites_at_most_one_chunk() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);
    mac.engine.set_backoff(Duration::ZERO);

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let bytes = sample_bytes(mib(3));
    let local_path = source.path().join("big.bin");
    std::fs::write(&local_path, &bytes).expect("the local file should write");
    let local_path_text = local_path.to_string_lossy().into_owned();

    let timed_push = |remote_name: &str| -> u64 {
        let before = mac.engine.wire_bytes();
        let id = mac
            .engine
            .push(
                phone_key.clone(),
                local_path_text.clone(),
                remote_name.to_owned(),
            )
            .expect("the push should be accepted");
        let engine = Arc::clone(&mac.engine);
        let wanted = id.clone();
        poll_until("the push to finish", move || {
            engine
                .transfers()
                .iter()
                .any(|t| t.id == wanted && t.state == TransferState::Done)
        });
        mac.engine.wire_bytes() - before
    };

    let clean = timed_push("Root/clean.bin");
    assert_eq!(
        std::fs::read(phone.shared_root().join("clean.bin")).expect("clean.bin should land"),
        bytes,
        "a clean push must land every byte"
    );

    // One chunk is exactly one mebibyte: `LocalFs::manifest`, on both sides,
    // always builds with `ChunkSize::one_mebibyte()`. The margin past that
    // covers one retry's own hello and manifest exchange, a few hundred
    // bytes at most for a three chunk file.
    let bound = clean + MIB + 200_000;

    for (label, n) in [
        ("early", clean / 20),
        ("mid", clean / 2),
        ("late", clean - 60),
    ] {
        mac.engine.set_cut(n.max(1));
        let name = format!("cut-{label}.bin");
        let used = timed_push(&format!("Root/{name}"));
        let landed = std::fs::read(phone.shared_root().join(&name))
            .unwrap_or_else(|_| panic!("a cut at {label} (n={n}) should still land {name}"));
        assert_eq!(landed, bytes, "a cut at {label} must still land every byte");
        assert!(
            used <= bound,
            "a cut at {label} (n={n}) used {used} wire bytes, the bound is {bound}; \
             more than one chunk must have been rewritten"
        );
    }

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// H1: a push that never verifies must fail, not retry forever.
// ---------------------------------------------------------------------------

/// A peer that answers every write, truncate, rename, and set-mtime with
/// success, but stores nothing: its manifest always reports the file as
/// present and empty, whatever was "written" to it. Stands in for a peer
/// that acknowledges everything and keeps none of it.
struct AcksButStoresNothingFs;

impl FileOps for AcksButStoresNothingFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
    }

    fn write(&self, _path: &RemotePath, _offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        Ok(u32::try_from(bytes.len()).unwrap_or(u32::MAX))
    }

    fn truncate(&self, _path: &RemotePath, _length: u64) -> Result<(), OpError> {
        Ok(())
    }

    fn rename(&self, _from: &RemotePath, _to: &RemotePath) -> Result<(), OpError> {
        Ok(())
    }

    fn set_mtime(&self, _path: &RemotePath, _modified_unix_secs: i64) -> Result<(), OpError> {
        Ok(())
    }

    fn mkdir(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn delete(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        // Always answers as if the file exists and is empty, no matter what
        // was written to it: this peer keeps nothing.
        Ok(manifest_from_bytes(&[], ChunkSize::one_mebibyte()))
    }
}

/// H1: `push.rs`'s `send` used to retry forever when a chunk it sent from
/// offset 0 never verified, because a peer like this always answers the
/// final manifest check with something that differs. Every attempt starts
/// from offset 0 again, since this peer's manifest never shows anything
/// landed, so the first attempt must already be fatal.
#[test]
fn a_push_to_a_peer_that_stores_nothing_fails_within_a_bounded_number_of_attempts() {
    let mac = build("Vamana");
    let peer = start_peer_with(&mac.key, Arc::new(AcksButStoresNothingFs));
    pair_with_peer(&mac, &peer);
    mac.engine.set_backoff(Duration::from_millis(10));

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    std::fs::write(&local_path, sample_bytes(1024)).expect("the local file should write");

    let id = mac
        .engine
        .push(
            key_hex(&peer.key),
            local_path.to_string_lossy().into_owned(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the push to fail rather than retry forever",
        |t| t.state == TransferState::Failed,
    );

    let info = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is still listed");
    assert_eq!(
        code_of_error(&info.error.expect("a failed transfer carries an error")),
        "TransferError::ChunkFailedVerification"
    );

    mac.engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// H4: a peer that claims to have written more than it was sent is fatal.
// ---------------------------------------------------------------------------

/// A peer that answers every write by claiming it wrote more bytes than
/// the call ever sent it. Stands in for a peer that does not speak the
/// write protocol honestly.
struct ClaimsExtraWrittenFs;

impl FileOps for ClaimsExtraWrittenFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::Unsupported)
    }

    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
    }

    fn write(&self, _path: &RemotePath, _offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        Ok(u32::try_from(bytes.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1))
    }

    fn truncate(&self, _path: &RemotePath, _length: u64) -> Result<(), OpError> {
        Ok(())
    }

    fn rename(&self, _from: &RemotePath, _to: &RemotePath) -> Result<(), OpError> {
        Ok(())
    }

    fn set_mtime(&self, _path: &RemotePath, _modified_unix_secs: i64) -> Result<(), OpError> {
        Ok(())
    }

    fn mkdir(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn delete(&self, _path: &RemotePath) -> Result<(), OpError> {
        Err(OpError::Unsupported)
    }

    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Ok(manifest_from_bytes(&[], ChunkSize::one_mebibyte()))
    }
}

/// H4: `write_all_remote` used to trust a peer's claimed write length past
/// what the call actually sent, which moved its own byte count ahead of
/// what really went out: the next piece was read from the wrong offset in
/// the local file and written to the wrong offset on the peer, bytes in
/// between silently skipped on both sides, caught only much later as a
/// plain `ChunkFailedVerification` once the whole file's manifest no
/// longer matches. A peer that claims more than it was sent must instead
/// fail the push cleanly and immediately, naming what actually went wrong.
#[test]
fn a_peer_that_claims_extra_bytes_written_fails_the_push_cleanly() {
    let mac = build("Vamana");
    let peer = start_peer_with(&mac.key, Arc::new(ClaimsExtraWrittenFs));
    pair_with_peer(&mac, &peer);

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    std::fs::write(&local_path, sample_bytes(1024)).expect("the local file should write");

    let id = mac
        .engine
        .push(
            key_hex(&peer.key),
            local_path.to_string_lossy().into_owned(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the push to fail cleanly rather than panic",
        |t| t.state == TransferState::Failed,
    );

    let info = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is still listed");
    assert_eq!(
        code_of_error(&info.error.expect("a failed transfer carries an error")),
        "OpError::Internal"
    );

    mac.engine.stop();
    peer.close();
}

/// H2: a push's stored record is trusted only while the local file still
/// matches the size and modified time recorded when its manifest was
/// built. Editing the file between two attempts of the same push must
/// land the edited bytes, not the stale ones the first attempt's manifest
/// described.
#[test]
fn editing_the_local_file_between_two_attempts_lands_the_edited_bytes() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);
    mac.engine.set_backoff(Duration::from_secs(2));

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("a.bin");
    let original = sample_bytes(mib(3));
    std::fs::write(&local_path, &original).expect("the local file should write");
    let local_path_text = local_path.to_string_lossy().into_owned();

    // Cut partway through the first attempt's sending: well past `build`
    // writing its Record::Ready for the original bytes, and well short of
    // landing the whole file.
    mac.engine.set_cut(MIB);

    let id = mac
        .engine
        .push(
            phone_key.clone(),
            local_path_text.clone(),
            "Root/a.bin".to_owned(),
        )
        .expect("the push should be accepted");

    wait_transfer(
        &mac,
        &id,
        "the cut attempt to fail and wait to retry",
        |t| t.state == TransferState::Paused,
    );

    // H3: the Write entry reflects only what an attempt actually sent, not
    // the whole file up front. The cut attempt above sent at most 1 MiB
    // before it failed, well under the 3 MiB file, so the log must not
    // already show the whole file as written.
    let logged_before_retry: u64 = mac
        .engine
        .access_log(None, 100)
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Write)
        .filter_map(|e| e.bytes)
        .sum();
    assert!(
        logged_before_retry < original.len() as u64,
        "the cut attempt must not log the whole file's size up front, got {logged_before_retry}"
    );

    // Edited between attempts: different content and a different size,
    // neither of which the first attempt's manifest describes any more.
    let edited = sample_bytes(mib(2) + 12_345);
    std::fs::write(&local_path, &edited).expect("the edited file should write");

    wait_transfer(&mac, &id, "the retried push to finish", |t| {
        t.state == TransferState::Done
    });

    assert_eq!(
        std::fs::read(phone.shared_root().join("a.bin")).expect("a.bin should have landed"),
        edited,
        "the push must land the edited bytes, not the stale ones from the first attempt"
    );

    mac.engine.stop();
    phone.engine.stop();
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
        side.download.path(),
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
        side.download.path(),
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
        side.download.path(),
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
// Batch D, item 2: a batch survives a restart, and forget removes its file.
// ---------------------------------------------------------------------------

/// Pair two real engines, the way `stop_returns_when_only_one_side_confirmed`
/// does. `pull_folder` needs a real `list`, which the hand-built `Peer` in
/// this file does not answer, so this test cannot use `pair_with_peer`.
fn pair_two_engines(phone: &Side, mac: &Side) -> String {
    phone.engine.set_reachable(true);
    phone.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.start_pairing_with(PairingMethod::Code);
    let phone_addr = loopback_addr(phone);
    mac.engine.offer_candidate(phone_addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    mac.engine
        .pick_candidate(format!("wifi:{phone_addr}"))
        .expect("the injected candidate should be pickable");
    mac.inbox.wait_pairing("the Mac's code", is_code);
    phone.inbox.wait_pairing("the phone's code", is_code);
    mac.engine.confirm_pairing(true);
    phone.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac's confirm", is_confirmed);
    phone
        .inbox
        .wait_pairing("the phone's confirm", is_confirmed);
    mac.engine.devices()[0].key_hex.clone()
}

#[test]
fn a_restart_keeps_a_batchs_grouping() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    std::fs::write(
        phone.shared_root().join("Camera/big.bin"),
        sample_bytes(mib(8)),
    )
    .expect("the phone's shared folder should accept a file");

    // `pull_folder`'s own listing dial never wraps its stream in `Cut`, only
    // a transfer attempt's dial does (`transfer::attempt`), so the cut armed
    // here waits untouched through the listing and lands on the one file's
    // own dial, the same technique `an_interrupted_first_pass_resumes_after_a_restart`
    // uses through a hand-built peer.
    mac.engine.set_cut(2 * MIB);

    let batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("the transfer to pause after the cut", move || {
        engine.transfers().iter().any(|t| {
            t.batch_id.as_deref() == Some(wanted.as_str()) && t.state == TransferState::Paused
        })
    });

    let before_ids: Vec<String> = mac
        .engine
        .transfers()
        .into_iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch_id.as_str()))
        .map(|t| t.id)
        .collect();
    assert_eq!(
        before_ids.len(),
        1,
        "the one file queued is the one transfer"
    );

    mac.engine.stop();
    phone.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let batch = engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch should still be listed after a restart");
    assert_eq!(
        batch.state,
        TransferState::Paused,
        "the batch's one surviving transfer is still paused"
    );
    let after_ids: Vec<String> = engine
        .transfers()
        .into_iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch.id.as_str()))
        .map(|t| t.id)
        .collect();
    assert_eq!(
        after_ids, before_ids,
        "the same transfer ids are still grouped under the batch"
    );

    engine.stop();
}

// ---------------------------------------------------------------------------
// Batch D audit, D4: a batch's done count survives a restart.
// ---------------------------------------------------------------------------

#[test]
fn a_batch_with_some_files_done_before_a_restart_still_reports_them_done_after() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    // Two files the same size, so it does not matter which one `set_cut`
    // below happens to land on: `pull_folder` queues both at once and a
    // pool of workers may dial either first, but whichever dial is not the
    // one `set_cut` is armed for runs uncut and reaches Done in full, while
    // the other pauses partway.
    std::fs::write(
        phone.shared_root().join("Camera/a.bin"),
        sample_bytes(mib(4)),
    )
    .expect("the phone's shared folder should accept the first file");
    std::fs::write(
        phone.shared_root().join("Camera/b.bin"),
        sample_bytes(mib(4)),
    )
    .expect("the phone's shared folder should accept the second file");

    // `set_cut` is one shot (`Option::take` in `transfer::attempt`), so only
    // the first dial to reach it is cut; the other transfer's dial finds
    // nothing armed and runs to completion. 2 MiB is short of each file's
    // 4 MiB, so the cut one pauses partway rather than finishing anyway.
    mac.engine.set_cut(2 * MIB);

    let batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("one file to finish and the other to pause", move || {
        let transfers = engine.transfers();
        let done = transfers
            .iter()
            .filter(|t| t.batch_id.as_deref() == Some(wanted.as_str()))
            .filter(|t| t.state == TransferState::Done)
            .count();
        let paused = transfers
            .iter()
            .filter(|t| t.batch_id.as_deref() == Some(wanted.as_str()))
            .filter(|t| t.state == TransferState::Paused)
            .count();
        done == 1 && paused == 1
    });
    let before_stop = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch is listed");
    assert_eq!(before_stop.files_done, 1, "one of the two files is done");

    mac.engine.stop();
    phone.engine.stop();

    // A new engine on the same folders, as if the app had been restarted.
    // The done file's own record does not survive: `run_once` removed it
    // the moment it finished. Only the paused file's record does.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    let surviving: Vec<_> = engine
        .transfers()
        .into_iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch_id.as_str()))
        .collect();
    assert_eq!(
        surviving.len(),
        1,
        "only the paused transfer's own row survives the restart"
    );

    // This is the audit's "0 of 100, then 40 of 100, forever": without the
    // stored floor, files_done would read 0 here, because the done file's
    // row is gone and the live count alone has nothing left to count it
    // from.
    let after = engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch is still listed after a restart");
    assert_eq!(
        after.files_done, 1,
        "the batch's stored floor still says one file is done, not zero"
    );

    engine.stop();
}

#[test]
fn forget_removes_the_devices_batch_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    std::fs::write(phone.shared_root().join("Camera/a.bin"), sample_bytes(1024))
        .expect("the phone's shared folder should accept a file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key.clone(), "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Done)
    });

    let batch_file = mac.data.path().join("batches").join(&batch_id);
    assert!(batch_file.exists(), "the batch record should be on disk");

    mac.engine
        .forget(phone_key)
        .expect("a paired device can be forgotten");
    assert!(
        !batch_file.exists(),
        "forget removes the device's batch files under data_dir/batches/"
    );
    assert!(
        mac.engine.batches().is_empty(),
        "the forgotten device's batches must leave the list"
    );

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Batch D audit, D5 and D6: a bound on a stored batch's count, and cleanup
// of one that does not decode.
// ---------------------------------------------------------------------------

#[test]
fn a_garbage_batch_file_is_removed_when_the_engine_starts() {
    let side = build("Vamana");
    side.engine.stop();

    let batches_dir = side.data.path().join("batches");
    // A name shaped like a real batch id, `<device key>-<session>`, so it is
    // not skipped for that reason first; its contents are what do not
    // decode.
    let garbage_path = batches_dir
        .join("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef-garbage");
    std::fs::write(&garbage_path, b"not a batch record").expect("the garbage file should write");
    assert!(
        garbage_path.exists(),
        "the garbage file exists before the restart"
    );

    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    )
    .expect("the engine should build even with a garbage batch file present");

    assert!(
        !garbage_path.exists(),
        "a batch file that does not decode is removed, not left for every future start to skip"
    );
    assert!(
        engine.batches().is_empty(),
        "the garbage file names no real batch"
    );
}

// ---------------------------------------------------------------------------
// Batch D audit, D8: behaviours the audit found missing tests for.
// ---------------------------------------------------------------------------

#[test]
fn pull_folder_creates_nothing_when_the_peer_folder_is_too_deep() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    // The folder passed to `pull_folder` is depth 1, so 32 more nested
    // folders below it reaches depth 33, one past the documented 32 level
    // cap (design/errors.json, Runtime::FolderTooLarge).
    let mut path = phone.shared_root().join("Camera");
    std::fs::create_dir(&path).expect("a folder for the camera roll");
    for _ in 0..32 {
        path.push("Sub");
        std::fs::create_dir(&path).expect("a nested folder");
    }
    std::fs::write(path.join("deep.jpg"), sample_bytes(16)).expect("a file at the bottom");

    let error = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect_err("a folder nested this deep should be refused");
    assert_eq!(code_of_error(&error), "Runtime::FolderTooLarge");
    assert!(
        mac.engine.batches().is_empty(),
        "a refused folder listing creates no batch"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "a refused folder listing creates no transfer"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// A filesystem with one file that always fails once it is fetched: `list`
/// finds it, but `stat` refuses it. Stands in for a peer whose one file
/// cannot be read, so the transfer it becomes reaches `Failed` at once.
struct AlwaysMissingFs;

impl FileOps for AlwaysMissingFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Ok((
            vec![Entry {
                name: "missing.bin".to_owned(),
                kind: FileKind::File,
                size: 4,
                modified_unix_secs: 1_000_000,
            }],
            None,
        ))
    }
    fn stat(&self, _path: &RemotePath) -> Result<Entry, OpError> {
        Err(OpError::NotFound)
    }
    fn read(&self, _path: &RemotePath, _offset: u64, _length: u32) -> Result<Vec<u8>, OpError> {
        Err(OpError::Unsupported)
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
    fn manifest(&self, _path: &RemotePath) -> Result<Manifest, OpError> {
        Err(OpError::Unsupported)
    }
}

#[test]
fn retry_keeps_a_transfers_batch_id_and_clears_the_batchs_end_time() {
    let side = build("Vamana");
    let peer = start_peer_with(&side.key, Arc::new(AlwaysMissingFs));
    pair_with_peer(&side, &peer);

    let batch_id = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect("the folder copy is accepted; its one file fails once fetched");

    let engine = Arc::clone(&side.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to fail", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Failed)
    });
    let failed_batch = side
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the failed batch is listed");
    assert!(
        failed_batch.ended_unix_secs.is_some(),
        "a batch with nothing left moving has an end time"
    );

    let transfer_id = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.batch_id.as_deref() == Some(batch_id.as_str()))
        .expect("the one transfer the batch covers")
        .id;

    side.engine
        .retry(transfer_id.clone())
        .expect("a failed transfer can be retried");

    wait_transfer(
        &side,
        &transfer_id,
        "retry to requeue it under the same batch",
        |t| t.state == TransferState::Queued && t.batch_id.as_deref() == Some(batch_id.as_str()),
    );
    let retried_batch = side
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the batch is still listed");
    assert_eq!(
        retried_batch.ended_unix_secs, None,
        "a batch with something moving again has no end time"
    );

    side.engine.stop();
    peer.close();
}

// ---------------------------------------------------------------------------
// docs/engine-contract.md item 16a: a first pass verifies every chunk
// against the peer's manifest as it lands, not only on a later resume.
// ---------------------------------------------------------------------------

/// A filesystem whose manifest is honest but whose `read` never matches it.
/// Stands in for a peer that serves a correct manifest and then sends the
/// wrong bytes for a chunk.
struct WrongChunkFs {
    bytes: Vec<u8>,
}

impl FileOps for WrongChunkFs {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        Ok(Entry {
            name: "big.bin".to_owned(),
            kind: FileKind::File,
            size: u64::try_from(self.bytes.len()).unwrap_or(0),
            modified_unix_secs: 1_000_000,
        })
    }

    fn read(&self, path: &RemotePath, _offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        // Every byte answered is wrong, on purpose: the manifest this peer
        // serves is honest, but nothing it reads back ever matches it.
        let want = usize::try_from(length).unwrap_or(0);
        Ok(vec![0xFFu8; want])
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

    fn manifest(&self, path: &RemotePath) -> Result<Manifest, OpError> {
        if path.as_str() != "big.bin" {
            return Err(OpError::NotFound);
        }
        Ok(manifest_from_bytes(&self.bytes, ChunkSize::one_mebibyte()))
    }
}

#[test]
fn a_peer_that_sends_a_wrong_chunk_in_the_first_pass_fails_verification() {
    let side = build("Vamana");
    let peer = start_peer_with(
        &side.key,
        Arc::new(WrongChunkFs {
            bytes: sample_bytes(mib(2)),
        }),
    );
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "wrong.bin");
    wait_transfer(&side, &id, "the transfer to fail verification", |t| {
        t.state == TransferState::Failed
    });

    let info = side
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed transfer is still listed");
    let error = info.error.expect("a failed transfer carries an error");
    assert_eq!(
        code_of_error(&error),
        "TransferError::ChunkFailedVerification"
    );
    let FerryError::Failed { detail, .. } = error;
    assert_eq!(
        detail.as_deref(),
        Some("0"),
        "the first chunk is the one that fails, and its index travels as the detail"
    );
    assert!(
        !side.download_root().join("wrong.bin").exists(),
        "nothing that fails verification is ever landed"
    );

    side.engine.stop();
    peer.close();
}

#[test]
fn a_batch_whose_transfers_all_finished_is_dropped_and_its_file_deleted_at_load() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Camera")).expect("a folder for the camera roll");
    std::fs::write(phone.shared_root().join("Camera/a.bin"), sample_bytes(1024))
        .expect("the phone's shared folder should accept a file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Done)
    });

    let batch_file = mac.data.path().join("batches").join(&batch_id);
    assert!(
        batch_file.exists(),
        "the batch record is on disk while the batch is still known"
    );

    mac.engine.stop();
    phone.engine.stop();

    // A new engine on the same folders. The one transfer reached Done, so
    // its own record does not survive: none of this batch's transfer ids
    // name a surviving row.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");

    assert!(
        engine.batches().iter().all(|b| b.id != batch_id),
        "a batch none of whose transfers survived is dropped at load"
    );
    assert!(
        !batch_file.exists(),
        "its file is removed at load, the same way a device's forgotten batches are"
    );

    engine.stop();
}

// ---------------------------------------------------------------------------
// Item 2 additions, I1: a batch reports its transport and error, and
// retry_batch retries every failed transfer in it.
// ---------------------------------------------------------------------------

#[test]
fn a_failed_batch_reports_its_error_and_retry_batch_requeues_it() {
    let side = build("Vamana");

    let unknown = side
        .engine
        .retry_batch("no-such-batch".to_owned())
        .expect_err("an unknown batch id cannot be retried");
    assert_eq!(code_of_error(&unknown), "Runtime::TransferNotFound");

    let peer = start_peer_with(&side.key, Arc::new(AlwaysMissingFs));
    pair_with_peer(&side, &peer);

    let batch_id = side
        .engine
        .pull_folder(key_hex(&peer.key), "Camera".to_owned())
        .expect("the folder copy is accepted; its one file fails once fetched");

    let engine = Arc::clone(&side.engine);
    let wanted = batch_id.clone();
    poll_until("the batch to fail", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted && b.state == TransferState::Failed)
    });
    let failed_batch = side
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the failed batch is listed");
    assert!(
        failed_batch.error.is_some(),
        "a Failed batch reports the failing transfer's error"
    );

    side.engine
        .retry_batch(batch_id.clone())
        .expect("a batch with a failed transfer can be retried");

    let engine = Arc::clone(&side.engine);
    let wanted = batch_id.clone();
    poll_until("retry_batch to requeue the failed transfer", move || {
        engine.transfers().iter().any(|t| {
            t.batch_id.as_deref() == Some(wanted.as_str()) && t.state == TransferState::Queued
        })
    });

    side.engine.stop();
    peer.close();
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
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange runs");

    let Side {
        engine,
        inbox,
        key: _key,
        data: _data,
        shared: _shared,
        download: _download,
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
    mac.engine.start_pairing_with(PairingMethod::Code);

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
            PairingState::Found { candidates, .. } => Some(candidates.len()),
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
    mac.engine.start_pairing_with(PairingMethod::Code);
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
    mac.engine.start_pairing_with(PairingMethod::Code);
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
    let mut hello = Vec::with_capacity(8);
    hello.extend_from_slice(&MAGIC);
    hello.extend_from_slice(&VERSION_MAX.to_be_bytes());
    hello.push(0); // Mode::Connect's wire byte.
    // A peer that is not welcome may close before this is written.
    drop(stream.write_all(&hello));
    drop(stream.flush());
    let mut answer = [0u8; 8];
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
    let mut expected = Vec::with_capacity(8);
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&VERSION_MAX.to_be_bytes());
    expected.push(0); // The responder's own mode byte is always this filler.

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
// docs/engine-contract.md item 16b: the responder tries every stored key in
// turn, so a second paired peer is not left unable to connect.
// ---------------------------------------------------------------------------

#[test]
fn an_engine_with_two_stored_peers_accepts_a_connection_from_the_second_with_no_address_hint() {
    let side = build("Vamana");
    side.engine.set_reachable(true);

    let first_peer = start_peer(&side.key, sample_bytes(1));
    pair_with_peer(&side, &first_peer);

    // `wait_pairing` finds the first matching state ever recorded, so
    // without this it would see the first pairing's own Found, Code, and
    // Confirmed states and never actually wait for the second one's.
    side.inbox.clear_pairings();
    let second_peer = start_peer(&side.key, sample_bytes(1));
    pair_with_peer(&side, &second_peer);

    // A fresh dial, from a socket the engine has never seen before: nothing
    // recorded favours the second peer's key over the first's. Before item
    // 16b, `choose_peer` guessed one candidate and a wrong guess failed the
    // handshake outright; the responder now tries every stored key against
    // the one message this dial sends.
    tcp::connect(
        loopback_addr(&side),
        &static_key(&second_peer.key),
        &public_key(&side.key),
    )
    .expect("the responder should try every stored key, not only the first");

    side.engine.stop();
    first_peer.close();
    second_peer.close();
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
    let landed = std::fs::read(side.download_root().join("big.bin")).expect("the file should land");
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

// ---------------------------------------------------------------------------
// Batch E, item 13: the access log.
// ---------------------------------------------------------------------------

#[test]
fn a_day_file_older_than_30_days_is_pruned_at_start() {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let access_log_dir = data.path().join("access_log");
    std::fs::create_dir_all(&access_log_dir).expect("the access log folder should be makeable");
    // 2020 is always more than 30 days before whenever this test runs.
    let old_day = access_log_dir.join("20200101");
    std::fs::write(&old_day, []).expect("a day file should write");

    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        generate_key().expect("a fresh key pair"),
        data.path(),
        shared.path(),
        download.path(),
        &inbox,
    )
    .expect("the engine should build");
    engine.start().expect("the engine should start");

    assert!(
        !old_day.exists(),
        "a day file over 30 days old must be pruned at start"
    );
    engine.stop();
}

#[test]
fn access_log_entries_survive_a_restart() {
    let side = build("Vamana");
    let peer = start_peer(&side.key, sample_bytes(16));
    pair_with_peer(&side, &peer);

    let id = pull_big(&side, &peer, "big.bin");
    wait_transfer(&side, &id, "the pull to finish", |t| {
        t.state == TransferState::Done
    });

    let before = side.engine.access_log(None, 10);
    let before_read = before
        .iter()
        .find(|e| e.verb == AccessVerb::Read && e.path == "big.bin")
        .expect("the pull should log a Read entry before the restart")
        .clone();

    side.engine.stop();
    peer.close();

    // A new engine on the same folder, as if the app had been restarted.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Vamana",
        side.key.clone(),
        side.data.path(),
        side.shared.path(),
        side.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");
    engine.start().expect("the engine should start");

    let after = engine.access_log(None, 10);
    let after_read = after
        .iter()
        .find(|e| e.id == before_read.id)
        .expect("the same entry should still be there after the restart");
    assert_eq!(
        after_read.bytes, before_read.bytes,
        "the byte count survives the restart"
    );
    assert_eq!(
        after_read.at_unix_secs, before_read.at_unix_secs,
        "the time survives the restart"
    );
    assert_eq!(
        after_read.device_key_hex, before_read.device_key_hex,
        "the device survives the restart"
    );

    engine.stop();
}

// ---------------------------------------------------------------------------
// Finding E1: stop must not lose an entry still pending when it runs.
// ---------------------------------------------------------------------------

#[test]
fn an_operation_served_just_before_stop_is_in_the_log_after_a_restart() {
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
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange runs");
    let mut client = Client::new(stream);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    client.read(&path, 0, 5).expect("a paired peer may read");

    // No sleep here: the read's entry is still pending in the roll-up, five
    // seconds short of its own idle timeout, and the connection is still
    // open. `stop` must still write it out rather than lose it.
    phone.engine.stop();

    // A new engine on the same folder, as if the app had been restarted.
    let inbox = Arc::new(Inbox::default());
    let engine = make_engine(
        "Pixel 3 XL",
        phone.key.clone(),
        phone.data.path(),
        phone.shared.path(),
        phone.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");
    engine.start().expect("the engine should start");

    let found = engine.access_log(None, 10);
    let entry = found
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .expect("the read served just before stop should still be in the log");
    assert_eq!(entry.path, "Root/note.txt");
    assert_eq!(entry.bytes, Some(5));

    engine.stop();
}

// ---------------------------------------------------------------------------
// Finding E8: access log behaviours the audit found untested.
// ---------------------------------------------------------------------------

/// Connect a paired peer to `phone` and finish the name exchange, ready to
/// send file operations. Shared by every finding E8 test below.
fn connect_paired_peer<F>(phone: &Side, peer: &Peer<F>) -> Client<SecureStream> {
    let connection = tcp::connect(
        loopback_addr(phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Fake", DeviceKind::Phone).expect("the name exchange runs");
    Client::new(stream)
}

#[test]
fn set_mtime_is_never_logged() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    client
        .set_mtime(&path, 1_700_000_000)
        .expect("a paired peer may set the mtime of a file it can write");

    // Close the connection so the served side finalises anything it had
    // pending, if there were anything to finalise.
    drop(client);
    std::thread::sleep(Duration::from_millis(300));

    assert!(
        phone.engine.access_log(None, 10).is_empty(),
        "set_mtime always follows a write that is already logged, so it logs nothing of its own"
    );
    phone.engine.stop();
}

#[test]
fn a_failed_operation_is_never_logged() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);

    let missing = RemotePath::parse("Root/missing.bin").expect("a valid path");
    match client.stat(&missing) {
        Err(RpcError::Remote(OpError::NotFound)) => {}
        other => panic!("a stat on a missing file should be refused, got {other:?}"),
    }

    let real = RemotePath::parse("Root/note.txt").expect("a valid path");
    client
        .stat(&real)
        .expect("a stat on a file that exists should succeed");

    drop(client);
    poll_until("the served stat to be logged", || {
        !phone.engine.access_log(None, 10).is_empty()
    });

    let found = phone.engine.access_log(None, 10);
    assert_eq!(
        found.len(),
        1,
        "only the successful stat is logged, not the failed one"
    );
    assert_eq!(found[0].path, "Root/note.txt");
    phone.engine.stop();
}

#[test]
fn rename_logs_the_destination_not_the_source() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("old.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);
    let from = RemotePath::parse("Root/old.txt").expect("a valid path");
    let to = RemotePath::parse("Root/new.txt").expect("a valid path");
    client
        .rename(&from, &to)
        .expect("a paired peer may rename a file it can write");

    drop(client);
    poll_until("the served rename to be logged", || {
        !phone.engine.access_log(None, 10).is_empty()
    });

    let found = phone.engine.access_log(None, 10);
    let entry = found
        .iter()
        .find(|e| e.verb == AccessVerb::Rename)
        .expect("the rename should be logged");
    assert_eq!(
        entry.path, "Root/new.txt",
        "the destination is logged, not the source"
    );
    phone.engine.stop();
}

#[test]
fn forget_keeps_the_devices_access_log_entries() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    std::fs::write(phone.shared_root().join("note.txt"), b"hello")
        .expect("the shared folder should accept a file");
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    let mut client = connect_paired_peer(&phone, &peer);
    let path = RemotePath::parse("Root/note.txt").expect("a valid path");
    client.read(&path, 0, 5).expect("a paired peer may read");
    drop(client);

    poll_until("the read to be logged before forget", || {
        !phone.engine.access_log(None, 10).is_empty()
    });
    let before = phone.engine.access_log(None, 10);
    assert_eq!(before.len(), 1);

    phone
        .engine
        .forget(key_hex(&peer.key))
        .expect("a paired device can be forgotten");

    let after = phone.engine.access_log(None, 10);
    assert_eq!(
        after.len(),
        1,
        "forget removes the device's peer entry and transfers, not its access log"
    );
    assert_eq!(after[0].id, before[0].id);
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 14: automatic copying.
//
// A `MemoryFs` peer gives exact control over names, sizes and content,
// which is what the skip rules turn on. The Mac dials it for `list`, for
// each `set_auto_copy(true)` while reachable, and for each run that follows
// from that: `pair_with_peer` alone does not make the Mac consider the peer
// reachable, the same way it does not in `two_engines.rs`, because the Mac
// is the side that dialled to pair and only the accepting side's own
// `mark_reachable` call ever fires from that connection.
// ---------------------------------------------------------------------------

/// Turn the switch off then on again, the alternative
/// `docs/engine-contract.md` item 14 names to a second reachability
/// transition, and wait for the run it starts to end.
/// The current Unix time, in whole seconds. `last_run_unix_secs` has this
/// same resolution, which is exactly what makes it possible for two runs to
/// share one value: see `toggle_and_wait_for_run`.
fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn toggle_and_wait_for_run(mac: &Side, device: &str, want_files: u32) {
    // G9: `last_run_files` alone can already equal `want_files` from the
    // run before this toggle, most often when two runs in a row both find
    // nothing new. Polling for it alone could then pass before this
    // toggle's own run has even started. `last_run_unix_secs` moving past
    // the value it held before the toggle proves a run actually finished
    // after it.
    let before_unix_secs = mac.engine.auto_copy(device.to_owned()).last_run_unix_secs;

    // A run against the fake peer these tests use can finish inside the
    // same second it started, since `last_run_unix_secs` only has one
    // second of resolution. Waiting here, before the toggle, for the wall
    // clock to move past whatever second `before_unix_secs` was itself
    // read at is what guarantees the run this toggle starts cannot also
    // land in that same second and tie with it forever.
    if let Some(before) = before_unix_secs {
        while current_unix_secs() <= before {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    mac.engine
        .set_auto_copy(device.to_owned(), false)
        .expect("turning the switch off should succeed");
    mac.engine
        .set_auto_copy(device.to_owned(), true)
        .expect("turning it back on should succeed");
    let engine = Arc::clone(&mac.engine);
    let wanted = device.to_owned();
    poll_until(
        "a new run to finish with the expected file count",
        move || {
            let info = engine.auto_copy(wanted.clone());
            info.last_run_unix_secs > before_unix_secs && info.last_run_files == Some(want_files)
        },
    );
}

#[test]
fn turning_on_automatic_copying_copies_new_photos_and_never_repeats_them() {
    let mac = build("Vamana");
    let fs = Arc::new(MemoryFs::new());
    let bytes_a = sample_bytes(500);
    fs.insert_file("Internal storage/DCIM/a.jpg", bytes_a.clone());
    let peer = start_peer_with(&mac.key, Arc::clone(&fs));
    pair_with_peer(&mac, &peer);
    let device = key_hex(&peer.key);

    mac.engine
        .list(device.clone(), String::new())
        .expect("listing the peer's roots should dial it and succeed");

    let before = mac.engine.auto_copy(device.clone());
    assert!(!before.enabled, "the switch starts off");

    mac.engine
        .set_auto_copy(device.clone(), true)
        .expect("a paired, reachable device should accept the switch");

    let engine = Arc::clone(&mac.engine);
    let wanted = device.clone();
    poll_until("the first run to copy the one file it found", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    let after_first = mac.engine.auto_copy(device.clone());
    assert!(after_first.enabled);
    assert_eq!(after_first.source, "Internal storage/DCIM");
    assert_eq!(
        after_first.destination,
        format!("{}/DCIM", mac.download_root().display())
    );
    assert_eq!(
        std::fs::read(mac.download_root().join("DCIM/a.jpg")).expect("a.jpg should have landed"),
        bytes_a
    );

    // Same device, same file, nothing changed: the next run finds nothing
    // new, and still records that it ran.
    toggle_and_wait_for_run(&mac, &device, 0);

    // The same content at a new path, as a rename on the phone leaves it:
    // skipped by root hash, not copied under its new name.
    fs.insert_file("Internal storage/DCIM/renamed.jpg", bytes_a.clone());
    toggle_and_wait_for_run(&mac, &device, 0);
    assert!(
        !mac.download_root().join("DCIM/renamed.jpg").exists(),
        "content already held under another name is not copied again"
    );

    // A file with new content is copied.
    let bytes_b = sample_bytes(700);
    fs.insert_file("Internal storage/DCIM/b.jpg", bytes_b.clone());
    toggle_and_wait_for_run(&mac, &device, 1);
    assert_eq!(
        std::fs::read(mac.download_root().join("DCIM/b.jpg")).expect("b.jpg should have landed"),
        bytes_b
    );

    mac.engine.stop();
    peer.close();
}

#[test]
fn a_manual_pull_writes_a_held_row_automatic_copying_will_not_repeat() {
    let mac = build("Vamana");
    let fs = Arc::new(MemoryFs::new());
    let bytes_a = sample_bytes(500);
    fs.insert_file("Internal storage/DCIM/a.jpg", bytes_a.clone());
    let bytes_b = sample_bytes(700);
    fs.insert_file("Internal storage/DCIM/b.jpg", bytes_b.clone());
    let peer = start_peer_with(&mac.key, Arc::clone(&fs));
    pair_with_peer(&mac, &peer);
    let device = key_hex(&peer.key);

    // A manual pull, before automatic copying is ever turned on, is the
    // same kind of completed pull a batch's own transfers are.
    let id = mac
        .engine
        .pull(
            device.clone(),
            "Internal storage/DCIM/a.jpg".to_owned(),
            "manual-a.jpg".to_owned(),
        )
        .expect("a manual pull should be accepted");
    wait_transfer(&mac, &id, "the manual pull to finish", |t| {
        t.state == TransferState::Done
    });

    mac.engine
        .set_auto_copy(device.clone(), true)
        .expect("a paired, reachable device should accept the switch");
    let engine = Arc::clone(&mac.engine);
    let wanted = device.clone();
    poll_until("the run to skip the manually pulled file", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    assert!(
        !mac.download_root().join("DCIM/a.jpg").exists(),
        "the manually pulled file is not copied again under DCIM"
    );
    assert_eq!(
        std::fs::read(mac.download_root().join("DCIM/b.jpg")).expect("b.jpg should have landed"),
        bytes_b
    );

    mac.engine.stop();
    peer.close();
}

#[test]
fn automatic_copying_and_the_held_index_survive_a_restart() {
    let mac = build("Vamana");
    let fs = Arc::new(MemoryFs::new());
    let bytes_a = sample_bytes(500);
    fs.insert_file("Internal storage/DCIM/a.jpg", bytes_a.clone());
    let peer = start_peer_with(&mac.key, Arc::clone(&fs));
    pair_with_peer(&mac, &peer);
    let device = key_hex(&peer.key);

    mac.engine
        .list(device.clone(), String::new())
        .expect("listing should succeed");
    mac.engine
        .set_auto_copy(device.clone(), true)
        .expect("the switch should turn on");
    let engine = Arc::clone(&mac.engine);
    let wanted = device.clone();
    poll_until("the first run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(1)
    });
    let before = mac.engine.auto_copy(device.clone());
    mac.engine.stop();

    let inbox = Arc::new(Inbox::default());
    let restarted = make_engine(
        "Vamana",
        mac.key.clone(),
        mac.data.path(),
        mac.shared.path(),
        mac.download.path(),
        &inbox,
    )
    .expect("the engine should build on the folder it left");
    restarted.start().expect("the engine should start");

    let after = restarted.auto_copy(device.clone());
    assert_eq!(
        after.enabled, before.enabled,
        "the switch survives a restart"
    );
    assert_eq!(
        after.last_run_unix_secs, before.last_run_unix_secs,
        "the last run time survives a restart"
    );
    assert_eq!(
        after.last_run_files, before.last_run_files,
        "the last run's file count survives a restart"
    );

    // The held index survived too: a fresh run over the same file, once the
    // restarted engine can reach the peer again, finds nothing new.
    restarted.offer_candidate(peer.addr);
    restarted
        .list(device.clone(), String::new())
        .expect("listing should succeed again after the restart");
    restarted
        .set_auto_copy(device.clone(), false)
        .expect("turning the switch off should succeed");
    restarted
        .set_auto_copy(device.clone(), true)
        .expect("turning it back on should succeed");
    let watched = Arc::clone(&restarted);
    let wanted = device.clone();
    poll_until("the post restart run to find nothing new", move || {
        watched.auto_copy(wanted.clone()).last_run_files == Some(0)
    });

    restarted.stop();
    peer.close();
}

#[test]
fn set_auto_copy_on_an_unknown_device_is_not_paired() {
    let mac = build("Vamana");
    let unknown = "ab".repeat(32);

    let error = mac
        .engine
        .set_auto_copy(unknown.clone(), true)
        .expect_err("an unpaired device should be refused");
    assert_eq!(code_of_error(&error), "Runtime::NotPaired");

    let info = mac.engine.auto_copy(unknown);
    assert!(
        !info.enabled,
        "an unknown device always answers as disabled, never as an error"
    );
    mac.engine.stop();
}
