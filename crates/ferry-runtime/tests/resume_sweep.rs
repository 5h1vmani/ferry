//! A deterministic sweep over where a link can break.
//!
//! Job 3 in `docs/jobs.md` promises that a dropped transfer continues, and
//! that a resume refetches at most one chunk however and whenever the link
//! broke. `engine_paths.rs` proves that promise at one moment: the link
//! breaks once, after the first chunk has landed. This file proves it at
//! every moment across a whole transfer.
//!
//! # How a cut is placed
//!
//! `Engine::set_cut(after_bytes)` arms a byte-accurate cut for the next dial.
//! `crates/ferry-runtime/src/guard.rs`'s `Cut` wrapper sits between the frame
//! layer and the encrypted stream, so the byte it counts is exactly the
//! plaintext frame byte defined by `crates/ferry-core/src/frame.rs` and
//! `docs/protocol.md` section 6: `[u32 payload_len][u8 kind][u32
//! request_id][payload]`. Nothing below that, such as the Noise transport
//! envelope, is visible to it or counted by it.
//!
//! # How the bound is derived
//!
//! Every number below is computed from that wire format, not guessed. A
//! frame costs nine header bytes plus its payload. A length-prefixed field,
//! such as a path or a chunk of file data, costs four bytes plus its own
//! bytes. From `crates/ferry-core/src/ops.rs`:
//!
//! - A `read` request payload is `opcode(1) + path(4+len) + offset(8) +
//!   length(4)`.
//! - A `read` response payload is `bytes(4+len)`, the four being the length
//!   prefix and `len` being the chunk's own bytes.
//! - A `stat` request payload is `opcode(1) + path(4+len)`.
//! - A `stat` response payload is one `Entry`: `name(4+len) + kind(1) +
//!   size(8) + modified(8)`.
//! - A `hello` payload is `name(4+len) + kind(1)`.
//!
//! `retry_wire()` is the cost paid again on every dial: one hello written by
//! this device, one hello read back from the peer, and one `stat` request
//! and response. `chunk_wire(len)` is what a cut can cause to be paid twice
//! for one chunk of `len` bytes: the chunk's own bytes, plus the frame bytes
//! of the `read` request that asked for it and the frame bytes of the `read`
//! response around it (its header and its length prefix; the chunk bytes
//! themselves are the first term, so they are not counted again here).
//! `max_chunk_wire()` is `chunk_wire` for the largest chunk in the file,
//! which is the most a single cut can cause to be paid twice.
//!
//! # Why the bound holds
//!
//! A cut lands during some chunk `k`, or earlier, during the hello or the
//! `stat`. Say the chunks before `k` already verified and were recorded; the
//! first attempt then spent `retry_wire() + sum(chunks before k) + partial`
//! bytes before it failed, where `partial` is at most `chunk_wire(k)` (chunk
//! `k`'s own full cost). The second attempt resumes at chunk `k` and repeats
//! `retry_wire() + sum(chunks from k on)`, in full, because nothing of chunk
//! `k` was ever recorded as verified. Adding the two:
//!
//! ```text
//! total = 2*retry_wire() + sum(all chunks) + partial
//!       = retry_wire() + clean + partial
//!       <= clean + retry_wire() + max_chunk_wire()
//! ```
//!
//! using `clean = retry_wire() + sum(all chunks)`. A cut inside the hello or
//! the `stat` itself never even gets a record written, so the second attempt
//! repeats the whole clean transfer and the total is `N + clean`, which is
//! covered by the same bound since `N` is then well under `retry_wire()`.
//!
//! # `restart - clean - 1`, measured
//!
//! The task that asked for this file also asks for a cut at N = 1 and the
//! quantity `restart - clean - 1`, calling it "the per-attempt overhead".
//! Measured here, it comes out at zero: a cut at byte 1 lands inside this
//! device's own outgoing hello, before a single byte of it has gone out, so
//! the first attempt writes no record and the second attempt is a bit for
//! bit repeat of a clean transfer. `restart` is then exactly `1 + clean`,
//! and the subtraction cancels. That is a true measurement, not a bug, and
//! it is reported below. It does not, though, bound a cut that lands after
//! a chunk has already been recorded as verified, because that case pays
//! `retry_wire()` on the second attempt without the first attempt ever
//! having paid it. `retry_wire()`, computed directly from the wire format
//! above, is what actually bounds those cuts, and is what the sweep checks
//! against.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind;
use ferry_core::rpc::{FileOps, exchange_hello, serve};
use ferry_core::tcp::{Listener, Pending};
use ferry_runtime::{
    Config, DeviceKind as RuntimeDeviceKind, Engine, EngineListener, KeyPair, PairingState, Root,
    TransferState, generate_key,
};

// ---------------------------------------------------------------------------
// The wire arithmetic. See the module documentation for the derivation.
// ---------------------------------------------------------------------------

/// This device's name in `hello`. Short and fixed, so the arithmetic below
/// has a fixed input.
const ENGINE_NAME: &str = "Sweep";

/// The fake peer's name in `hello`.
const PEER_NAME: &str = "Fake";

/// The one file the fake peer serves, and its name on both sides.
const FILE_NAME: &str = "big.bin";

/// The chunk size this test asks the engine to use.
const CHUNK_SIZE: u32 = 1024;

/// The source file's length: four whole chunks and a short tail.
const FILE_LEN: usize = 4396;

/// The frame header: `[u32 payload_len][u8 kind][u32 request_id]`. See
/// `crates/ferry-core/src/frame.rs`.
const FRAME_HEADER: u64 = 9;

/// The length prefix `Encoder::bytes` and `Encoder::text` both write before
/// a byte string or a piece of text. See `crates/ferry-core/src/wire.rs`.
const LEN_PREFIX: u64 = 4;

/// A frame's wire cost, given its payload's length.
fn frame_bytes(payload_len: u64) -> u64 {
    FRAME_HEADER + payload_len
}

/// The wire cost of one side's `hello` frame.
fn hello_bytes(name: &str) -> u64 {
    // name(4+len) + kind(1)
    frame_bytes(LEN_PREFIX + name.len() as u64 + 1)
}

/// The wire cost of one `read` request frame.
fn read_request_bytes() -> u64 {
    // opcode(1) + path(4+len) + offset(8) + length(4)
    frame_bytes(1 + (LEN_PREFIX + FILE_NAME.len() as u64) + 8 + 4)
}

/// The wire cost of a `read` response frame around one chunk, not counting
/// the chunk's own bytes: the frame header plus the length prefix in front
/// of the chunk.
fn read_response_overhead() -> u64 {
    FRAME_HEADER + LEN_PREFIX
}

/// The wire cost of the `stat` request frame.
fn stat_request_bytes() -> u64 {
    // opcode(1) + path(4+len)
    frame_bytes(1 + LEN_PREFIX + FILE_NAME.len() as u64)
}

/// The wire cost of the `stat` response frame: one `Entry`.
fn stat_response_bytes() -> u64 {
    // name(4+len) + kind(1) + size(8) + modified(8)
    frame_bytes(LEN_PREFIX + FILE_NAME.len() as u64 + 1 + 8 + 8)
}

/// The cost paid again on every dial: both hellos, and one `stat` call.
fn retry_wire() -> u64 {
    hello_bytes(ENGINE_NAME) + hello_bytes(PEER_NAME) + stat_request_bytes() + stat_response_bytes()
}

/// One chunk's own bytes, and the request and response frame bytes around
/// it, for a chunk of `chunk_len` bytes.
fn chunk_wire(chunk_len: u64) -> u64 {
    chunk_len + read_request_bytes() + read_response_overhead()
}

/// The length of every chunk the source file is split into, in order.
fn chunk_lengths() -> Vec<u64> {
    let mut left = FILE_LEN as u64;
    let mut out = Vec::new();
    while left > 0 {
        let this = left.min(u64::from(CHUNK_SIZE));
        out.push(this);
        left -= this;
    }
    out
}

/// The predicted wire cost of one clean pull, computed from the wire format.
fn predicted_clean() -> u64 {
    retry_wire()
        + chunk_lengths()
            .iter()
            .map(|&len| chunk_wire(len))
            .sum::<u64>()
}

/// The largest single chunk cost, which bounds what one cut can cause to be
/// paid twice.
fn max_chunk_wire() -> u64 {
    chunk_lengths()
        .iter()
        .map(|&len| chunk_wire(len))
        .max()
        .unwrap_or(0)
}

/// The wire-byte offset, from the start of a pull's own dial, at which each
/// chunk's response finishes arriving. One entry per chunk, in order.
fn chunk_boundaries() -> Vec<u64> {
    let mut at = retry_wire();
    chunk_lengths()
        .iter()
        .map(|&len| {
            at += chunk_wire(len);
            at
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A minimal harness: one engine, one hand-built peer, paired.
//
// This mirrors the shape `engine_paths.rs` uses (a peer built from
// `ferry-core`'s own `tcp`, `noise`, and `rpc` primitives), trimmed to what
// this file needs: no slow reads, no capped reads, and a single fixed file.
// ---------------------------------------------------------------------------

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(10);

/// How often a poll looks again. Kept small, because the fast sweep runs
/// hundreds of pulls and the poll granularity is on its critical path.
const POLL_TICK: Duration = Duration::from_micros(200);

/// Bytes that are easy to check and hard to get right by accident.
fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap_or(0))
        .collect()
}

/// What one engine has told the app so far.
#[derive(Default)]
struct Notes {
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
        self.lock().pairings.push(state);
        self.ready.notify_all();
    }

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

/// The listener the one engine under test is given.
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

/// The one engine under test, and the folders it owns.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    key: KeyPair,
    #[allow(dead_code)] // Held so the folder is not deleted while used.
    data: tempfile::TempDir,
    #[allow(dead_code)] // Held so the folder is not deleted while used.
    shared: tempfile::TempDir,
    download: tempfile::TempDir,
}

impl Side {
    /// Where a pull lands. Never the same folder as the served root.
    fn download_root(&self) -> &Path {
        self.download.path()
    }
}

/// Build and start one engine on fresh folders.
fn build() -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let engine = Engine::new(
        Config {
            data_dir: data.path().to_string_lossy().into_owned(),
            shared_roots: vec![Root {
                name: "Root".to_owned(),
                path: shared.path().to_string_lossy().into_owned(),
                writable: true,
            }],
            download_dir: download.path().to_string_lossy().into_owned(),
            display_name: ENGINE_NAME.to_owned(),
            listen_port: 0,
            key: key.clone(),
            kind: RuntimeDeviceKind::Mac,
        },
        Box::new(Recorder {
            inbox: Arc::clone(&inbox),
        }),
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

fn is_found(state: &PairingState) -> bool {
    matches!(state, PairingState::Found { .. })
}

fn is_code(state: &PairingState) -> bool {
    matches!(state, PairingState::Code { .. })
}

fn is_confirmed(state: &PairingState) -> bool {
    matches!(state, PairingState::Confirmed { .. })
}

/// A filesystem that serves one fixed file, and nothing else.
struct OneFile {
    bytes: Vec<u8>,
}

impl FileOps for OneFile {
    fn list(&self, _path: &RemotePath, _cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        Err(OpError::Unsupported)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        if path.as_str() != FILE_NAME {
            return Err(OpError::NotFound);
        }
        Ok(Entry {
            name: FILE_NAME.to_owned(),
            kind: FileKind::File,
            size: u64::try_from(self.bytes.len()).unwrap_or(0),
            modified_unix_secs: 1_000_000,
        })
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        if path.as_str() != FILE_NAME {
            return Err(OpError::NotFound);
        }
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        let want = usize::try_from(length).unwrap_or(0);
        let end = start.saturating_add(want).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
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

/// A fake peer: listens, pairs once, then serves `OneFile` on every
/// connection after that until it is closed.
///
/// The engine dials it fresh on every retry. Its accept loop keeps running
/// across connections, so it is already ready for a new one once an earlier
/// one ends: this is the same shape `engine_paths.rs`'s `Peer` uses, and
/// `a_transfer_pauses_when_the_link_breaks_and_finishes_when_it_returns`
/// there already confirms it works. Nothing here breaks the peer's own side
/// of the link; the cut lives entirely on the engine's dialed stream.
struct Peer {
    addr: SocketAddr,
    key: KeyPair,
    expect_pair: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
}

fn static_key(key: &KeyPair) -> StaticKey {
    StaticKey::from_stored(&key.private, &key.public).expect("a stored key pair should load")
}

fn public_key(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

fn key_hex(key: &KeyPair) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for byte in &key.public {
        write!(out, "{byte:02x}").expect("a string always accepts more text");
    }
    out
}

fn start_peer(engine_key: &KeyPair) -> Peer {
    let listener = Listener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("the fake peer should bind a port");
    let addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        listener.local_addr().port(),
    );
    let key = generate_key().expect("a fresh key pair for the peer");
    let fs = Arc::new(OneFile {
        bytes: sample_bytes(FILE_LEN),
    });
    let peer = Peer {
        addr,
        key: key.clone(),
        expect_pair: Arc::new(AtomicBool::new(false)),
        closing: Arc::new(AtomicBool::new(false)),
    };

    let engine_public = public_key(engine_key);
    let expect_pair = Arc::clone(&peer.expect_pair);
    let closing = Arc::clone(&peer.closing);
    drop(std::thread::spawn(move || {
        while !closing.load(Ordering::SeqCst) {
            let Ok(pending) = listener.accept() else {
                continue;
            };
            if closing.load(Ordering::SeqCst) {
                return;
            }
            let key = static_key(&key);
            let fs = Arc::clone(&fs);
            let pairing = expect_pair.swap(false, Ordering::SeqCst);
            drop(std::thread::spawn(move || {
                serve_one(pending, &key, engine_public, &fs, pairing);
            }));
        }
    }));
    peer
}

fn serve_one(
    pending: Pending,
    key: &StaticKey,
    engine: PublicKey,
    fs: &Arc<OneFile>,
    pairing: bool,
) {
    let mut stream: Box<dyn ReadWrite> = if pairing {
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
    if exchange_hello(&mut stream, PEER_NAME, DeviceKind::Phone).is_err() {
        return;
    }
    drop(serve(&mut stream, fs.as_ref()));
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

impl Peer {
    fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        drop(TcpStream::connect_timeout(
            &self.addr,
            Duration::from_secs(2),
        ));
    }
}

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

fn pull_to(side: &Side, peer: &Peer, local_name: &str) -> String {
    side.engine
        .pull(
            key_hex(&peer.key),
            FILE_NAME.to_owned(),
            local_name.to_owned(),
        )
        .expect("the pull should be accepted")
}

/// Wait until `check` is true, looking again every [`POLL_TICK`].
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

fn wait_done(side: &Side, id: &str) {
    let engine = Arc::clone(&side.engine);
    let wanted = id.to_owned();
    poll_until("the transfer to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted && t.state == TransferState::Done)
    });
}

// ---------------------------------------------------------------------------
// One pull, measured.
// ---------------------------------------------------------------------------

/// Run one pull to `local_name`, wait for it to finish, and return the wire
/// bytes it cost and the landed file's bytes.
fn timed_pull(side: &Side, peer: &Peer, local_name: &str) -> (u64, Vec<u8>) {
    let before = side.engine.wire_bytes();
    let id = pull_to(side, peer, local_name);
    wait_done(side, &id);
    let used = side.engine.wire_bytes() - before;
    let landed =
        std::fs::read(side.download_root().join(local_name)).expect("the file should land");
    (used, landed)
}

/// Set up one paired engine and peer, with the chunk size and backoff this
/// sweep needs. The caller is responsible for `side.engine.stop()` and
/// `peer.close()`.
fn setup(backoff: Duration) -> (Side, Peer, Vec<u8>) {
    let side = build();
    let peer = start_peer(&side.key);
    pair_with_peer(&side, &peer);
    side.engine.set_backoff(backoff);
    side.engine
        .set_chunk_size(CHUNK_SIZE)
        .expect("1024 is a valid chunk size");
    let source = sample_bytes(FILE_LEN);
    (side, peer, source)
}

/// Cut at `n`, pull to a fresh name, and assert it lands correctly and
/// within `bound` wire bytes. Prints `n` and the bytes used on failure.
fn assert_cut_resumes(side: &Side, peer: &Peer, source: &[u8], n: u64, bound: u64, name: &str) {
    side.engine.set_cut(n);
    let (used, landed) = timed_pull(side, peer, name);
    assert_eq!(landed, source, "cut at N={n} landed the wrong bytes");
    assert!(
        used <= bound,
        "cut at N={n} used {used} wire bytes, the bound is {bound}"
    );
}

// ---------------------------------------------------------------------------
// The sweep.
// ---------------------------------------------------------------------------

#[test]
fn every_cut_near_a_chunk_boundary_resumes_and_refetches_at_most_one_chunk() {
    let (side, peer, source) = setup(Duration::ZERO);

    let (clean, landed) = timed_pull(&side, &peer, "clean.bin");
    assert_eq!(landed, source, "a clean pull must land every byte");
    let predicted = predicted_clean();
    assert_eq!(
        clean, predicted,
        "the wire format arithmetic must predict the clean pull exactly"
    );

    side.engine.set_cut(1);
    let (restart, landed) = timed_pull(&side, &peer, "restart.bin");
    assert_eq!(landed, source, "a cut at N=1 must still resume correctly");
    let overhead = i128::from(restart) - i128::from(clean) - 1;
    println!(
        "clean={clean} restart(N=1)={restart} restart-clean-1={overhead} retry_wire={} chunk_wire={}",
        retry_wire(),
        max_chunk_wire()
    );

    let bound = clean + retry_wire() + max_chunk_wire();

    let mut points: Vec<u64> = (1..=96).collect();
    for boundary in chunk_boundaries() {
        let boundary = i64::try_from(boundary).unwrap_or(i64::MAX);
        for delta in [-8i64, 8] {
            let n = boundary + delta;
            if n >= 1 {
                points.push(u64::try_from(n).unwrap_or(1));
            }
        }
    }
    for i in 0..24u64 {
        let n = 1 + (i * clean) / 24;
        if n >= 1 && n < clean {
            points.push(n);
        }
    }
    points.sort_unstable();
    points.dedup();

    let started = Instant::now();
    for (i, n) in points.iter().enumerate() {
        assert_cut_resumes(&side, &peer, &source, *n, bound, &format!("n{i}.bin"));
    }
    let took = started.elapsed();
    println!(
        "every_cut_near_a_chunk_boundary_resumes_and_refetches_at_most_one_chunk: {} points in {took:?}",
        points.len()
    );

    side.engine.stop();
    peer.close();
}

#[test]
#[ignore = "runs on demand: cargo test -p ferry-runtime --test resume_sweep -- --ignored"]
fn every_cut_across_the_whole_transfer_resumes_and_refetches_at_most_one_chunk() {
    let (side, peer, source) = setup(Duration::ZERO);

    let (clean, landed) = timed_pull(&side, &peer, "clean.bin");
    assert_eq!(landed, source, "a clean pull must land every byte");

    let bound = clean + retry_wire() + max_chunk_wire();

    let started = Instant::now();
    for n in 1..clean {
        assert_cut_resumes(&side, &peer, &source, n, bound, &format!("n{n}.bin"));
    }
    let took = started.elapsed();
    println!(
        "every_cut_across_the_whole_transfer_resumes_and_refetches_at_most_one_chunk: {} points ({clean} clean bytes) in {took:?}",
        clean - 1
    );

    side.engine.stop();
    peer.close();
}
