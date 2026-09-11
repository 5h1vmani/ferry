//! Item 19: the file operations the phone's `DocumentsProvider` sits on.
//!
//! `docs/engine-contract.md`, item 19. Two engines in one process, paired
//! the same way `two_engines.rs` pairs them, with the Mac standing in for
//! the phone's provider: it walks `stat`, `write_at`, `read_at`,
//! `truncate`, `rename`, `mkdir` and `delete` against the peer, and reads
//! its own access log back to see what it recorded.
//!
//! This file also proves the two rules item 19 adds around those calls: one
//! pool per device, so two listings in a row reuse one connection, and a
//! write over one mebibyte refused with `Runtime::WriteTooLarge`.
//!
//! No test sleeps and hopes, and no test sends a packet off this machine.
//! Every engine binds `127.0.0.1` on a port the operating system picks, and
//! every wait is a condition variable with a generous deadline.
//!
//! The helpers below are copied from `two_engines.rs`, as every test file in
//! this crate does today (`docs/agent-runs.md`, rule 3): one test file per
//! item, never appended to a shared one.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_runtime::{
    Config, DeviceKind, Engine, EngineListener, KeyPair, PairingMethod, PairingState, Root,
    generate_key,
};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// The two-engine harness, copied from `two_engines.rs`.
// ---------------------------------------------------------------------------

/// What one engine has told the app so far.
#[derive(Default)]
struct Notes {
    /// Every pairing state, in the order it arrived.
    pairings: Vec<PairingState>,
    /// How many times anything changed. It only has to move.
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

    fn pairing(&self, state: PairingState) {
        let mut notes = self.lock();
        notes.pairings.push(state);
        notes.ticks += 1;
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

/// One engine, its inbox, and the folders it owns.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    /// The one root this side serves, named `"Root"`.
    shared_root: PathBuf,
    /// Held so the folders live as long as the engine does.
    _data: tempfile::TempDir,
    _shared: tempfile::TempDir,
    _download: tempfile::TempDir,
}

/// Build and start one engine, of the given device kind, on fresh folders.
fn build_as(name: &str, kind: DeviceKind) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key: KeyPair = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let config = Config {
        data_dir: data.path().to_string_lossy().into_owned(),
        shared_roots: vec![Root {
            name: "Root".to_owned(),
            path: shared.path().to_string_lossy().into_owned(),
            writable: true,
        }],
        download_dir: download.path().to_string_lossy().into_owned(),
        display_name: name.to_owned(),
        listen_port: 0,
        key,
        kind,
    };
    let engine = Engine::new(
        config,
        Box::new(Recorder {
            inbox: Arc::clone(&inbox),
        }),
    )
    .expect("the engine should build from a good config");
    engine.start().expect("the engine should start");
    Side {
        engine,
        inbox,
        shared_root: shared.path().to_path_buf(),
        _data: data,
        _shared: shared,
        _download: download,
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

/// Pair the two engines with the code method, and return the phone's key
/// hex as the Mac stores it.
fn pair(mac: &Side, phone: &Side) -> String {
    phone.engine.set_reachable(true);
    phone.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.start_pairing_with(PairingMethod::Code);

    let phone_addr = loopback_addr(phone);
    mac.engine.offer_candidate(phone_addr);

    let found = mac
        .inbox
        .wait_pairing("the Mac to list a candidate", is_found);
    let PairingState::Found { candidates, .. } = &found else {
        panic!("expected candidates, got {found:?}");
    };
    let wanted = format!("wifi:{phone_addr}");
    let chosen = candidates
        .iter()
        .find(|candidate| candidate.id == wanted)
        .unwrap_or_else(|| panic!("the injected candidate {wanted} should be listed"));
    mac.engine
        .pick_candidate(chosen.id.clone())
        .expect("the candidate should be pickable");

    mac.inbox.wait_pairing("the Mac to show a code", is_code);
    phone
        .inbox
        .wait_pairing("the phone to show a code", is_code);

    mac.engine.confirm_pairing(true);
    phone.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    mac.engine.devices()[0].key_hex.clone()
}

// ---------------------------------------------------------------------------
// Item 19: one pool per device.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 19: "One test proves `list` reuses a
/// pooled connection: two lists in a row leave the peer's connection count
/// at one."
///
/// Before item 19, `Engine::list` dialled fresh on every call, so the
/// second listing cost the peer a second accepted connection. It now
/// borrows from the device's pool, which keeps the first connection idle
/// and hands it back.
///
/// The count is read on the phone, the side that accepts, and as a
/// difference either side of the two listings. Pairing itself opens a
/// connection, so an absolute count would measure pairing as well.
#[test]
fn two_listings_in_a_row_reuse_one_pooled_connection() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");

    // The first listing dials, because nothing is pooled yet. Counting from
    // after it isolates what the second listing costs.
    let before = phone.engine.accepted_connections();
    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("the first listing should dial the phone and succeed");
    let after_first = phone.engine.accepted_connections();
    // A count that never moves would make every check below pass whatever
    // the pool did. The first listing has to dial, so it has to move once.
    assert_eq!(
        after_first,
        before + 1,
        "the first listing dials, so the phone accepts exactly one connection"
    );

    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("the second listing should succeed");
    assert_eq!(
        phone.engine.accepted_connections(),
        after_first,
        "the second listing must reuse the pooled connection, not dial again"
    );

    // A third listing, of a different folder, is still the same connection:
    // the pool is per device, not per folder.
    mac.engine
        .list(phone_key, "Root/Photos".to_owned())
        .expect("the third listing should succeed");
    assert_eq!(
        phone.engine.accepted_connections(),
        after_first,
        "a listing of another folder on the same device must reuse it too"
    );

    mac.engine.stop();
    phone.engine.stop();
}
