//! Two engines in one process, pairing and moving a file.
//!
//! This is the happy path of the acceptance gate. It runs the phone's side
//! and the Mac's side of every step that works: pairing with a code both
//! sides show, a device list that survives the pairing, a real file pulled
//! across a real socket, a forget that clears the device and its records,
//! and a stop that returns quickly. It also proves the three refusals the
//! app relies on: an unknown transfer, an unpaired device, and a path that
//! climbs out of the shared root.
//!
//! No test sleeps and hopes. Every wait is a condition variable with a
//! generous deadline, so a slow machine makes the test slower, never flaky.
//!
//! # What this file does not cover
//!
//! Everything that goes wrong lives in `engine_paths.rs`: a stop in the
//! middle of a transfer, a confirm on one side only, a forget while a peer
//! is connected, a first pass cut short and resumed after a restart, a
//! candidate picked twice, a confirm after the pairing watchdog gave up, a
//! link that breaks and comes back, a device that is not reachable, and a
//! second engine on one data folder. That file also builds peers by hand,
//! which is the only way to test a peer that misbehaves.
//!
//! Neither file covers USB, which needs `adb`, a cable and a phone, nor real
//! mDNS, because a test machine may sit on a network that refuses multicast.
//! Both are in `docs/manual-checks.md`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_runtime::{
    Config, Engine, EngineListener, KeyPair, PairingState, TransferState, generate_key,
};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(10);

/// How big the file the Mac pulls is.
const FILE_BYTES: usize = 300 * 1024;

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

    /// Wait until `check` is true, rechecking on every callback.
    fn wait_until(&self, what: &str, check: impl Fn() -> bool) {
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

/// One engine, its inbox, and the folders it owns.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    shared_root: std::path::PathBuf,
    /// Held so the folders live as long as the engine does.
    _data: tempfile::TempDir,
    _shared: tempfile::TempDir,
}

fn build(name: &str) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let key: KeyPair = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let config = Config {
        data_dir: data.path().to_string_lossy().into_owned(),
        shared_root: shared.path().to_string_lossy().into_owned(),
        display_name: name.to_owned(),
        listen_port: 0,
        key,
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
    }
}

/// The address another engine in this process can dial.
fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
}

/// The six digits, or a panic saying what arrived instead.
fn code_of(state: &PairingState) -> String {
    match state {
        PairingState::Code { code } => code.clone(),
        other => panic!("expected a code, got {other:?}"),
    }
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

/// Bytes that are easy to check and hard to get right by accident.
fn sample_bytes() -> Vec<u8> {
    (0..FILE_BYTES)
        .map(|i| u8::try_from((i * 31 + 7) % 251).unwrap_or(0))
        .collect()
}

#[test]
fn two_engines_pair_and_move_a_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");

    // The phone is the side that waits. The Mac is the side that looks.
    phone.engine.set_reachable(true);
    phone.engine.start_pairing();
    mac.engine.start_pairing();

    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);

    let found = mac
        .inbox
        .wait_pairing("the Mac to list a candidate", is_found);
    let PairingState::Found { candidates } = &found else {
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

    let mac_code = code_of(&mac.inbox.wait_pairing("the Mac to show a code", is_code));
    let phone_code = code_of(
        &phone
            .inbox
            .wait_pairing("the phone to show a code", is_code),
    );
    assert_eq!(
        mac_code, phone_code,
        "both screens must show the same six digits"
    );
    assert_eq!(mac_code.len(), 6, "the code is six digits");

    mac.engine.confirm_pairing(true);
    phone.engine.confirm_pairing(true);
    mac.inbox.wait_pairing("the Mac to confirm", is_confirmed);
    phone
        .inbox
        .wait_pairing("the phone to confirm", is_confirmed);

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    assert_eq!(on_mac[0].name, "Pixel 3 XL");
    let on_phone = phone.engine.devices();
    assert_eq!(on_phone.len(), 1, "the phone lists the Mac");
    assert_eq!(on_phone[0].name, "Vamana");

    // A real file, over a real socket, verified chunk by chunk.
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let id = mac
        .engine
        .pull(
            on_mac[0].key_hex.clone(),
            "holiday.bin".to_owned(),
            "holiday.bin".to_owned(),
        )
        .expect("the pull should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_id = id.clone();
    mac.inbox.wait_until("the transfer to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_id && t.state == TransferState::Done)
    });

    let landed = std::fs::read(mac.shared_root.join("holiday.bin"))
        .expect("the file should be in the Mac's shared folder");
    assert_eq!(landed, bytes, "every byte must match");

    let leftovers: Vec<String> = std::fs::read_dir(&mac.shared_root)
        .expect("the Mac's shared folder should be readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("part"))
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "no partial file should be left behind, found {leftovers:?}"
    );

    // Forgetting takes the device out of the list and takes its transfer
    // records off the disk.
    let phone_key = on_mac[0].key_hex.clone();
    mac.engine
        .forget(phone_key)
        .expect("a paired device can be forgotten");
    assert!(
        mac.engine.devices().is_empty(),
        "the forgotten device must leave the list"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "its transfers must go with it"
    );

    let started = Instant::now();
    mac.engine.stop();
    phone.engine.stop();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "stop took {took:?}");

    // Calling stop twice must be safe.
    mac.engine.stop();
    phone.engine.stop();
}

#[test]
fn the_mac_lists_a_folder_on_the_phone() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");

    phone.engine.set_reachable(true);
    phone.engine.start_pairing();
    mac.engine.start_pairing();

    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);

    let found = mac
        .inbox
        .wait_pairing("the Mac to list a candidate", is_found);
    let PairingState::Found { candidates } = &found else {
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

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    let phone_key = on_mac[0].key_hex.clone();

    // A remote path always names at least one component, so the shared
    // root itself has no path of its own. `Photos` stands in for the first
    // folder a person opens.
    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("Photos/holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let entries = mac
        .engine
        .list(phone_key.clone(), "Photos".to_owned())
        .expect("the folder should list");
    let found_entry = entries
        .iter()
        .find(|entry| entry.name == "holiday.bin")
        .expect("the sample file should be in the listing");
    assert_eq!(
        found_entry.size,
        bytes.len() as u64,
        "the listed size must match the file on disk"
    );

    let missing = mac
        .engine
        .list(phone_key, "no-such-folder".to_owned())
        .expect_err("a folder that does not exist cannot be listed");
    assert_eq!(code_of_error(&missing), "OpError::NotFound");

    mac.engine.stop();
    phone.engine.stop();
}

/// The code an error carries, or a panic saying it had none.
fn code_of_error(error: &ferry_runtime::FerryError) -> String {
    let ferry_runtime::FerryError::Failed { code, .. } = error;
    code.clone()
}

#[test]
fn the_engine_refuses_what_it_should_and_says_why() {
    let side = build("Vamana");

    let missing = side
        .engine
        .retry("no-such-transfer".to_owned())
        .expect_err("an unknown transfer cannot be retried");
    assert_eq!(code_of_error(&missing), "Runtime::TransferNotFound");

    let stranger = side
        .engine
        .forget("not a key".to_owned())
        .expect_err("an unpaired device cannot be forgotten");
    assert_eq!(code_of_error(&stranger), "Runtime::NotPaired");

    let bad_path = side
        .engine
        .pull("00".repeat(32), "../escape".to_owned(), "a.bin".to_owned())
        .expect_err("a path that climbs out of the root is refused");
    assert_eq!(code_of_error(&bad_path), "PathError::ParentComponent");

    let unpaired = side
        .engine
        .pull("00".repeat(32), "a.bin".to_owned(), "a.bin".to_owned())
        .expect_err("an unpaired device cannot be pulled from");
    assert_eq!(code_of_error(&unpaired), "Runtime::NotPaired");

    side.engine.stop();
}

#[test]
fn a_bad_config_is_refused_before_anything_starts() {
    let data = tempfile::tempdir().expect("a temporary folder");
    let shared = tempfile::tempdir().expect("a temporary folder");
    let inbox = Arc::new(Inbox::default());
    let make = |name: String, key: KeyPair| {
        Engine::new(
            Config {
                data_dir: data.path().to_string_lossy().into_owned(),
                shared_root: shared.path().to_string_lossy().into_owned(),
                display_name: name,
                listen_port: 0,
                key,
            },
            Box::new(Recorder {
                inbox: Arc::clone(&inbox),
            }),
        )
    };

    let short_key = KeyPair {
        private: vec![0u8; 31],
        public: vec![0u8; 32],
    };
    let bad_key = make("Vamana".to_owned(), short_key).err().expect("refused");
    assert_eq!(code_of_error(&bad_key), "NoiseError::BadKeyLength");

    let good_key = generate_key().expect("a fresh key pair");
    let long_name = make("n".repeat(65), good_key).err().expect("refused");
    assert_eq!(code_of_error(&long_name), "Runtime::NameTooLong");
}
