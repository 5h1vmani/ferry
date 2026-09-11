//! `docs/engine-contract.md` item 5, the fix pass's guard extended to
//! `push_files`.
//!
//! `push` already refuses a second live push to the same destination on the
//! same device with `Runtime::PushInFlight` (`crates/ferry-runtime/src/push.rs`,
//! `live_push_exists`), because two live pushes to one path would race to
//! write the same `.ferry-part` file. `push_files` had no such guard: two
//! batches to the same folder, or one batch whose own local paths map to the
//! same remote name, could still collide. This file is that guard's own
//! test file, one file per item, per `docs/agent-runs.md` rule 3.
//!
//! Small helpers are copied from `engine_paths.rs` rather than imported
//! across test files, per that file's own convention.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_runtime::{
    BatchInfo, Config, DeviceKind as RuntimeDeviceKind, Engine, EngineListener, FerryError,
    KeyPair, PairingMethod, PairingState, Root, TransferState, generate_key,
};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How often a poll looks again.
const POLL_TICK: Duration = Duration::from_millis(10);

/// One mebibyte.
const MIB: u64 = 1024 * 1024;

/// That many mebibytes, as a byte count a `Vec` can use.
fn mib(count: u64) -> usize {
    usize::try_from(count * MIB).unwrap_or(usize::MAX)
}

/// Bytes that are easy to check and hard to get right by accident.
fn sample_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap_or(0))
        .collect()
}

/// The code an error carries, or a panic saying it had none.
fn code_of_error(error: &FerryError) -> String {
    let FerryError::Failed { code, .. } = error;
    code.clone()
}

// ---------------------------------------------------------------------------
// The listener the tests give each engine.
// ---------------------------------------------------------------------------

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

/// Wait until `check` is true, looking again every few milliseconds.
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

fn is_found(state: &PairingState) -> bool {
    matches!(state, PairingState::Found { .. })
}

fn is_code(state: &PairingState) -> bool {
    matches!(state, PairingState::Code { .. })
}

fn is_confirmed(state: &PairingState) -> bool {
    matches!(state, PairingState::Confirmed { .. })
}

// ---------------------------------------------------------------------------
// Engines under test.
// ---------------------------------------------------------------------------

/// One engine, its inbox, and the folders it owns.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    #[allow(dead_code)]
    key: KeyPair,
    #[allow(dead_code)]
    data: tempfile::TempDir,
    shared: tempfile::TempDir,
    #[allow(dead_code)]
    download: tempfile::TempDir,
}

impl Side {
    /// The one root this side serves, named `"Root"`.
    fn shared_root(&self) -> &std::path::Path {
        self.shared.path()
    }
}

/// Build and start one engine on fresh folders.
fn build(name: &str) -> Side {
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
            display_name: name.to_owned(),
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

/// The address another engine in this process can dial.
fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
}

/// Pair two real engines with a code, and return the phone's key as the Mac
/// knows it. Mirrors `engine_paths.rs`'s helper of the same name.
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

/// Every batch's id, for a plain length or membership check.
fn batch_ids(mac: &Side) -> Vec<String> {
    mac.engine
        .batches()
        .into_iter()
        .map(|b: BatchInfo| b.id)
        .collect()
}

/// Wait until one batch satisfies `check`.
fn wait_batch(mac: &Side, id: &str, what: &str, check: impl Fn(&BatchInfo) -> bool) {
    let engine = Arc::clone(&mac.engine);
    let wanted = id.to_owned();
    poll_until(what, move || {
        engine.batches().iter().any(|b| b.id == wanted && check(b))
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A second `push_files` call into the same folder, while the first batch is
/// still live, collides on the same destination name and must be refused
/// whole -- no batch, no transfer rows -- exactly as a second single `push`
/// to that name already is. Once the first batch finishes, the same call
/// succeeds: the guard is about two live pushes at once, never about a name
/// a finished push once used.
#[test]
fn a_second_push_files_call_that_collides_with_a_live_batch_is_refused() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Uploads"))
        .expect("a folder on the phone to push into");

    let source_one = tempfile::tempdir().expect("a folder for the first push's files");
    let path_a = source_one.path().join("shared.bin");
    std::fs::write(&path_a, sample_bytes(mib(1))).expect("shared.bin should write");

    let first_batch = mac
        .engine
        .push_files(
            phone_key.clone(),
            vec![path_a.to_string_lossy().into_owned()],
            "Root/Uploads".to_owned(),
        )
        .expect("the first push_files call should be accepted");

    // A second source folder, so the colliding local path is not literally
    // the same string as the first -- only its leaf name, and so its
    // destination inside "Root/Uploads", matches.
    let source_two = tempfile::tempdir().expect("a folder for the second push's files");
    let path_b = source_two.path().join("shared.bin");
    std::fs::write(&path_b, sample_bytes(mib(1))).expect("the colliding file should write");

    let second = mac.engine.push_files(
        phone_key.clone(),
        vec![path_b.to_string_lossy().into_owned()],
        "Root/Uploads".to_owned(),
    );
    assert_eq!(
        second.err().map(|error| code_of_error(&error)),
        Some("Runtime::PushInFlight".to_owned()),
        "a push_files call that collides with a live push must be refused whole"
    );

    assert_eq!(
        batch_ids(&mac),
        vec![first_batch.clone()],
        "the refused call must create no batch of its own"
    );
    assert_eq!(
        mac.engine.transfers().len(),
        1,
        "the refused call must create no transfer row of its own"
    );

    wait_batch(&mac, &first_batch, "the first batch to finish", |b| {
        b.state == TransferState::Done
    });

    // Once the first batch has finished, "Root/Uploads/shared.bin" is free
    // again: this is about two live pushes racing, never about a name a
    // finished push once used.
    let third_batch = mac
        .engine
        .push_files(
            phone_key,
            vec![path_b.to_string_lossy().into_owned()],
            "Root/Uploads".to_owned(),
        )
        .expect("the same call should succeed once the first batch has finished");
    wait_batch(&mac, &third_batch, "the third batch to finish", |b| {
        b.state == TransferState::Done
    });

    mac.engine.stop();
    phone.engine.stop();
}

/// Two local paths whose leaf names collide inside the same
/// `remote_folder` would race each other into the same `.ferry-part` file
/// just as surely as two separate live pushes would. The whole call is
/// refused, and nothing is queued: not even the file whose name did not
/// collide with anything.
#[test]
fn push_files_with_local_paths_sharing_a_leaf_name_is_refused() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair_two_engines(&phone, &mac);

    std::fs::create_dir(phone.shared_root().join("Uploads"))
        .expect("a folder on the phone to push into");

    let source_one = tempfile::tempdir().expect("a folder for the first file");
    let path_a = source_one.path().join("dup.bin");
    std::fs::write(&path_a, sample_bytes(mib(1))).expect("dup.bin should write");

    let source_two = tempfile::tempdir().expect("a folder for the second file");
    let path_b = source_two.path().join("dup.bin");
    std::fs::write(&path_b, sample_bytes(mib(1))).expect("the second dup.bin should write");

    let result = mac.engine.push_files(
        phone_key,
        vec![
            path_a.to_string_lossy().into_owned(),
            path_b.to_string_lossy().into_owned(),
        ],
        "Root/Uploads".to_owned(),
    );
    assert_eq!(
        result.err().map(|error| code_of_error(&error)),
        Some("Runtime::PushInFlight".to_owned()),
        "two local paths that map to the same remote name must refuse the whole call"
    );

    assert!(
        mac.engine.batches().is_empty(),
        "a call refused before anything is queued must create no batch"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "a call refused before anything is queued must create no transfer row"
    );

    mac.engine.stop();
    phone.engine.stop();
}
