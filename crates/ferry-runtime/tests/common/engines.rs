//! Two engines in one process, and the waits a test makes on them.
//!
//! Five test files each held their own copy of this harness. The copies had
//! drifted: one counted the access log callback apart from the rest, one
//! kept its temporary folders under named fields so it could restart an
//! engine on them, and the wait budget was ten seconds in two files and
//! thirty in three. This file is the union of those copies, so every test
//! waits the longest of the old budgets and reads the same `Side`.
//!
//! Each test binary that says `mod common;` compiles this whole file, so a
//! binary that uses only part of it would otherwise warn about the rest.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_runtime::{
    Config, DeviceKind, Engine, EngineListener, KeyPair, PairingMethod, PairingState, Root,
    generate_key,
};

/// How long any wait may take before the test gives up.
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Notes {
    /// Every pairing state, in the order it arrived.
    pub(crate) pairings: Vec<PairingState>,
    /// How many times anything changed. It only has to move.
    pub(crate) ticks: u64,
    /// How many times `access_log_changed` fired, counted apart from
    /// `ticks` so a test can tell that call apart from the others.
    pub(crate) access_log_ticks: u64,
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

    pub(crate) fn access_log_tick(&self) {
        self.lock().access_log_ticks += 1;
        self.ready.notify_all();
    }

    pub(crate) fn pairing(&self, state: PairingState) {
        let mut notes = self.lock();
        notes.pairings.push(state);
        notes.ticks += 1;
        drop(notes);
        self.ready.notify_all();
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

    /// Wait until `check` is true, rechecking on every callback.
    pub(crate) fn wait_until(&self, what: &str, check: impl Fn() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        let mut notes = self.lock();
        loop {
            if check() {
                return;
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
        self.inbox.access_log_tick();
    }
}

/// One engine, its inbox, and the folders it owns.
pub(crate) struct Side {
    pub(crate) engine: Arc<Engine>,
    pub(crate) inbox: Arc<Inbox>,
    pub(crate) key: KeyPair,
    /// The one root this side serves, named `"Root"`.
    pub(crate) shared_root: PathBuf,
    /// Where a pull lands. Never the same folder as `shared_root`.
    pub(crate) download_root: PathBuf,
    /// Held so the folders live as long as the engine does. Named rather
    /// than underscored, because a test that restarts an engine builds the
    /// next one on these same folders.
    pub(crate) data: tempfile::TempDir,
    pub(crate) shared: tempfile::TempDir,
    pub(crate) download: tempfile::TempDir,
}

/// Build and start one engine, of the given device kind, on fresh folders.
pub(crate) fn build_as(name: &str, kind: DeviceKind) -> Side {
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
        key: key.clone(),
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
        key,
        shared_root: shared.path().to_path_buf(),
        download_root: download.path().to_path_buf(),
        data,
        shared,
        download,
    }
}

/// Build and start one engine on fresh folders, as a Mac.
pub(crate) fn build(name: &str) -> Side {
    build_as(name, DeviceKind::Mac)
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

/// Pair the two engines with the code method, and return the phone's key
/// hex as the Mac stores it.
pub(crate) fn pair(mac: &Side, phone: &Side) -> String {
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

/// The code an error carries, or a panic saying it had none.
pub(crate) fn code_of_error(error: &ferry_runtime::FerryError) -> String {
    let ferry_runtime::FerryError::Failed { code, .. } = error;
    code.clone()
}
