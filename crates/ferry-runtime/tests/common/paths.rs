//! A peer built by hand, and the engine the tests point at it.
//!
//! The engine has no seam for a peer that misbehaves, so these tests speak
//! the wire themselves, out of the same parts the engine uses: `tcp`,
//! `noise`, `rpc` and `frame` from `ferry-core`. A peer that never sends
//! `hello`, a peer that answers one byte at a time, and a stranger that
//! speaks no Ferry at all are all built here.
//!
//! This harness was one block at the top of `engine_paths.rs`. That file is
//! now several, one per concern, and they share this.
//!
//! Each test binary that says `mod common;` compiles this whole file, so a
//! binary that uses only part of it would otherwise warn about the rest.

#![allow(dead_code)]

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_core::chunk::{ChunkSize, Manifest, manifest_from_bytes};
use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind;
use ferry_core::rpc::{FileOps, exchange_hello, serve};
use ferry_core::tcp::{Listener, Pending};
use ferry_runtime::{
    Config, DeviceKind as RuntimeDeviceKind, Engine, EngineListener, FerryError, KeyPair,
    PairingMethod, PairingState, Root, generate_key,
};

/// How long any wait may take before the test gives up.
pub(crate) const PATIENCE: Duration = Duration::from_secs(20);

/// How often a poll looks again.
pub(crate) const POLL_TICK: Duration = Duration::from_millis(10);

/// One mebibyte, which is also the chunk size the engine uses.
pub(crate) const MIB: u64 = 1024 * 1024;

/// That many mebibytes, as a byte count a `Vec` can use.
pub(crate) fn mib(count: u64) -> usize {
    usize::try_from(count * MIB).unwrap_or(usize::MAX)
}

/// The listener the tests give each engine.
///
/// What one engine has told the app so far.
#[derive(Default)]
pub(crate) struct Notes {
    /// Every pairing state, in the order it arrived.
    pub(crate) pairings: Vec<PairingState>,
    /// How many times anything changed.
    pub(crate) ticks: u64,
}

/// Collects callbacks and lets the test wait for one.
#[derive(Default)]
pub(crate) struct Inbox {
    pub(crate) notes: Mutex<Notes>,
    pub(crate) ready: Condvar,
}

impl Inbox {
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Notes> {
        self.notes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn tick(&self) {
        self.lock().ticks += 1;
        self.ready.notify_all();
    }

    pub(crate) fn ticks(&self) -> u64 {
        self.lock().ticks
    }

    pub(crate) fn pairing(&self, state: PairingState) {
        let mut notes = self.lock();
        notes.pairings.push(state);
        notes.ticks += 1;
        drop(notes);
        self.ready.notify_all();
    }

    /// Every pairing state seen so far.
    pub(crate) fn states(&self) -> Vec<PairingState> {
        self.lock().pairings.clone()
    }

    /// How many pairing states seen so far match `want`.
    pub(crate) fn count(&self, want: impl Fn(&PairingState) -> bool) -> usize {
        self.lock().pairings.iter().filter(|s| want(s)).count()
    }

    /// Forget every pairing state seen so far.
    ///
    /// `wait_pairing` finds the first recorded state `want` accepts, and
    /// never forgets one on its own. Pairing a second peer with the same
    /// engine needs this first, or `wait_pairing` matches the first
    /// pairing's own `Found`, `Code`, or `Confirmed` state instead of
    /// waiting for the second one's.
    pub(crate) fn clear_pairings(&self) {
        self.lock().pairings.clear();
    }

    /// Wait until a pairing state that `want` accepts has arrived.
    pub(crate) fn wait_pairing(
        &self,
        what: &str,
        want: impl Fn(&PairingState) -> bool,
    ) -> PairingState {
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
pub(crate) struct Recorder {
    pub(crate) inbox: Arc<Inbox>,
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

/// Poll `check` every `tick` until it answers true, or until `patience` runs
/// out. Returns whether `check` succeeded before the deadline.
///
/// The one loop every wait in this crate's tests that has no callback to
/// wait on is built from. [`poll_until`] and [`poll_until_or_describe`] are
/// this with [`PATIENCE`] and [`POLL_TICK`] already filled in; a file whose
/// wait is on a hot path, such as `resume_sweep.rs`, calls this directly
/// with its own tighter constants instead, since a shared tick that suits
/// an ordinary test is too coarse for a sweep of hundreds of pulls.
pub(crate) fn poll_every(patience: Duration, tick: Duration, check: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(tick);
    }
    false
}

/// Wait until `check` is true, looking again every few milliseconds.
///
/// Some of what these tests watch, such as the byte count of a transfer,
/// moves without a callback, because callbacks are rationed. So this polls
/// instead of waiting on the condition variable.
pub(crate) fn poll_until(what: &str, check: impl Fn() -> bool) {
    assert!(
        poll_every(PATIENCE, POLL_TICK, check),
        "waited {PATIENCE:?} for {what}"
    );
}

/// Like [`poll_until`], but the panic also carries what `describe` saw at
/// the deadline, so a failure on a slower machine says which state the
/// engine was in instead of only how long the test waited.
pub(crate) fn poll_until_or_describe(
    what: &str,
    check: impl Fn() -> bool,
    describe: impl Fn() -> String,
) {
    assert!(
        poll_every(PATIENCE, POLL_TICK, check),
        "waited {PATIENCE:?} for {what}; saw: {}",
        describe()
    );
}

/// Engines under test.
///
/// One engine, its inbox, and the folders it owns.
pub(crate) struct Side {
    pub(crate) engine: Arc<Engine>,
    pub(crate) inbox: Arc<Inbox>,
    pub(crate) key: KeyPair,
    pub(crate) data: tempfile::TempDir,
    pub(crate) shared: tempfile::TempDir,
    pub(crate) download: tempfile::TempDir,
}

impl Side {
    /// The one root this side serves, named `"Root"`.
    pub(crate) fn shared_root(&self) -> &Path {
        self.shared.path()
    }

    /// Where a pull lands. Never the same folder as `shared_root`.
    pub(crate) fn download_root(&self) -> &Path {
        self.download.path()
    }
}

/// Build an engine on the given folders. It is not started.
pub(crate) fn make_engine(
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
pub(crate) fn build(name: &str) -> Side {
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
pub(crate) fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
}

pub(crate) fn is_code(state: &PairingState) -> bool {
    matches!(state, PairingState::Code { .. })
}

pub(crate) fn is_confirmed(state: &PairingState) -> bool {
    matches!(state, PairingState::Confirmed { .. })
}

pub(crate) fn is_found(state: &PairingState) -> bool {
    matches!(state, PairingState::Found { .. })
}

pub(crate) fn is_failed(state: &PairingState) -> bool {
    matches!(state, PairingState::Failed { .. })
}

/// The code an error carries, or a panic saying it had none.
pub(crate) fn code_of_error(error: &FerryError) -> String {
    let FerryError::Failed { code, .. } = error;
    code.clone()
}

/// Bytes that are easy to check and hard to get right by accident.
pub(crate) fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap_or(0))
        .collect()
}

/// A peer built by hand.
///
/// The engine has no seam for a peer that misbehaves, so these tests speak the
/// real protocol: the version exchange, Noise, `hello`, and the file
/// operations layer, all from `ferry-core`.
///
/// The one file the fake peer serves, and how slowly it answers.
pub(crate) struct Script {
    /// The bytes of `big.bin`.
    pub(crate) bytes: Vec<u8>,
    /// The most bytes one read may answer with. Zero means the whole range.
    pub(crate) read_cap: u32,
    /// Reads at or past this offset wait `slow_pause` before they answer.
    pub(crate) slow_from: u64,
    /// How long a slow read waits.
    pub(crate) slow_pause: Duration,
}

/// A filesystem that serves one file, as slowly as the script says.
pub(crate) struct ScriptedFs {
    pub(crate) script: Mutex<Script>,
    /// How many reads have been answered.
    pub(crate) reads: AtomicU64,
}

impl ScriptedFs {
    pub(crate) fn new(bytes: Vec<u8>) -> Arc<Self> {
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

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Answer at most `cap` bytes per read, so one chunk takes many reads.
    pub(crate) fn cap_reads(&self, cap: u32) {
        self.lock().read_cap = cap;
    }

    /// Wait `pause` on every read at or past `from`.
    pub(crate) fn slow_down(&self, from: u64, pause: Duration) {
        let mut script = self.lock();
        script.slow_from = from;
        script.slow_pause = pause;
    }

    /// Answer every read at once again, in full.
    pub(crate) fn speed_up(&self) {
        let mut script = self.lock();
        script.slow_from = u64::MAX;
        script.slow_pause = Duration::ZERO;
        script.read_cap = 0;
    }

    /// How many reads have been answered so far.
    pub(crate) fn reads(&self) -> u64 {
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
pub(crate) struct Cutting<S> {
    pub(crate) inner: S,
    pub(crate) cut: Arc<AtomicBool>,
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
pub(crate) struct Peer<F> {
    /// Where the engine dials it.
    pub(crate) addr: SocketAddr,
    /// Its long lived key, as the app would store it.
    pub(crate) key: KeyPair,
    /// What it serves.
    pub(crate) fs: Arc<F>,
    /// True while the next connection is a pairing handshake.
    pub(crate) expect_pair: Arc<AtomicBool>,
    /// How long the peer waits before it sends its name.
    pub(crate) hello_delay: Arc<Mutex<Duration>>,
    /// True while the link is broken.
    pub(crate) cut: Arc<AtomicBool>,
    /// Set to end the accept loop.
    pub(crate) closing: Arc<AtomicBool>,
}

/// Rebuild the Noise key from the bytes the app would have stored.
pub(crate) fn static_key(key: &KeyPair) -> StaticKey {
    StaticKey::from_stored(&key.private, &key.public).expect("a stored key pair should load")
}

/// The public half, as `ferry-core` names it.
pub(crate) fn public_key(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

/// The 64 hex characters an engine knows a device by.
pub(crate) fn key_hex(key: &KeyPair) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for byte in &key.public {
        write!(out, "{byte:02x}").expect("a string always accepts more text");
    }
    out
}

/// Start a peer that serves one file, as slowly as the script says.
pub(crate) fn start_peer(engine_key: &KeyPair, bytes: Vec<u8>) -> Peer<ScriptedFs> {
    start_peer_with(engine_key, ScriptedFs::new(bytes))
}

/// Start a peer that answers on its own port, serving whatever `fs` says.
pub(crate) fn start_peer_with<F: FileOps + Send + Sync + 'static>(
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
pub(crate) fn serve_one<F: FileOps + Send + Sync>(
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
pub(crate) trait ReadWrite: Read + Write {}

impl<T: Read + Write> ReadWrite for T {}

impl<F> Peer<F> {
    /// Break the link: drop what is connected, refuse what arrives.
    pub(crate) fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
    }

    /// Answer again.
    pub(crate) fn mend(&self) {
        self.cut.store(false, Ordering::SeqCst);
    }

    /// Stop the accept loop, which closes the port.
    pub(crate) fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        // One connection, so the blocked accept returns and sees the flag.
        drop(TcpStream::connect_timeout(
            &self.addr,
            Duration::from_secs(2),
        ));
    }
}

/// Pair one engine with a hand-driven peer, and leave the peer able to serve.
pub(crate) fn pair_with_peer<F>(side: &Side, peer: &Peer<F>) {
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
pub(crate) fn pull_big<F>(side: &Side, peer: &Peer<F>, local_name: &str) -> String {
    side.engine
        .pull(
            key_hex(&peer.key),
            "big.bin".to_owned(),
            local_name.to_owned(),
        )
        .expect("the pull should be accepted")
}

/// Wait until one transfer satisfies `check`.
pub(crate) fn wait_transfer(
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

/// Batch D, item 2: a batch survives a restart, and forget removes its file.
///
/// Pair two real engines, the way `stop_returns_when_only_one_side_confirmed`
/// does. `pull_folder` needs a real `list`, which the hand-built `Peer` in
/// this file does not answer, so this test cannot use `pair_with_peer`.
pub(crate) fn pair_two_engines(phone: &Side, mac: &Side) -> String {
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
