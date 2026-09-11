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
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Aliased: `ferry_runtime::DeviceKind`, used unaliased below, is the
// boundary enum `Config` and `DeviceInfo` carry. This is `ferry-core`'s own,
// needed only for the raw `exchange_hello` call in the no-reconnect test.
use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::ops::OpError;
use ferry_core::path::RemotePath;
use ferry_core::peers::DeviceKind as CoreDeviceKind;
use ferry_core::rpc::{Client, RpcError, exchange_hello};
use ferry_core::tcp;
use ferry_runtime::{
    AccessVerb, Actor, Config, DeviceKind, Engine, EngineListener, KeyPair, PairingState, Root,
    TransferState, Transport, generate_key,
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
    /// How many times `access_log_changed` fired, counted apart from
    /// `ticks` so a test can tell that call apart from the others.
    access_log_ticks: u64,
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

    fn access_log_tick(&self) {
        self.lock().access_log_ticks += 1;
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

    fn access_log_changed(&self) {
        self.inbox.access_log_tick();
    }
}

/// One engine, its inbox, and the folders it owns.
struct Side {
    engine: Arc<Engine>,
    inbox: Arc<Inbox>,
    key: KeyPair,
    /// The one root this side serves, named `"Root"`.
    shared_root: PathBuf,
    /// Where a pull lands. Never the same folder as `shared_root`.
    download_root: PathBuf,
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
        _data: data,
        _shared: shared,
        _download: download,
    }
}

/// Build and start one engine on fresh folders, as a Mac.
fn build(name: &str) -> Side {
    build_as(name, DeviceKind::Mac)
}

/// The address another engine in this process can dial.
fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
}

/// A `Side`'s key, as `ferry-core` names it. For a raw connection built by
/// hand, bypassing `Engine::list`, so a test can issue more than one
/// operation on the very same connection.
fn static_key(key: &KeyPair) -> StaticKey {
    StaticKey::from_stored(&key.private, &key.public).expect("a stored key pair should load")
}

/// The public half, as `ferry-core` names it.
fn public_key(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

/// The six digits, or a panic saying what arrived instead.
fn code_of(state: &PairingState) -> String {
    match state {
        PairingState::Code { code, .. } => code.clone(),
        other => panic!("expected a code, got {other:?}"),
    }
}

/// Now, in Unix seconds, for checking a reported deadline is plausible.
fn now_unix_secs() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock should be after the epoch")
        .as_secs();
    i64::try_from(secs).expect("the current time fits in an i64")
}

/// The pairing timeout is two minutes. A little slack either side covers the
/// time a test itself takes to reach the point it checks a deadline.
fn assert_expires_about_two_minutes_out(expires_unix_secs: i64) {
    let now = now_unix_secs();
    assert!(
        (0..=130).contains(&(expires_unix_secs - now)),
        "the deadline should be about two minutes out, got {expires_unix_secs}, now is {now}"
    );
}

fn is_waiting(state: &PairingState) -> bool {
    matches!(state, PairingState::Waiting { .. })
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
// One long, linear narrative is the point of this test: every step of the
// happy path, in the order a person walks through it. Splitting it into
// smaller functions would hide that it is one path, not several.
#[allow(clippy::too_many_lines)]
fn two_engines_pair_and_move_a_file() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);

    // The phone is the side that waits. The Mac is the side that looks.
    phone.engine.set_reachable(true);
    phone.engine.start_pairing();
    mac.engine.start_pairing();

    // Item 10: every pairing state that has a deadline states it, including
    // Waiting, the state a side that is not looking for anyone starts in.
    let waiting = phone
        .inbox
        .wait_pairing("the phone to wait for pairing", is_waiting);
    let PairingState::Waiting {
        expires_unix_secs: waiting_expires,
    } = waiting
    else {
        panic!("expected Waiting, got {waiting:?}");
    };
    assert_expires_about_two_minutes_out(waiting_expires);

    let phone_addr = loopback_addr(&phone);
    mac.engine.offer_candidate(phone_addr);

    let found = mac
        .inbox
        .wait_pairing("the Mac to list a candidate", is_found);
    let PairingState::Found {
        candidates,
        expires_unix_secs: found_expires,
    } = &found
    else {
        panic!("expected candidates, got {found:?}");
    };
    assert_expires_about_two_minutes_out(*found_expires);
    let wanted = format!("wifi:{phone_addr}");
    let chosen = candidates
        .iter()
        .find(|candidate| candidate.id == wanted)
        .unwrap_or_else(|| panic!("the injected candidate {wanted} should be listed"));
    mac.engine
        .pick_candidate(chosen.id.clone())
        .expect("the candidate should be pickable");

    let mac_code_state = mac.inbox.wait_pairing("the Mac to show a code", is_code);
    let phone_code_state = phone
        .inbox
        .wait_pairing("the phone to show a code", is_code);
    for state in [&mac_code_state, &phone_code_state] {
        let PairingState::Code {
            expires_unix_secs, ..
        } = state
        else {
            panic!("expected a code, got {state:?}");
        };
        assert_expires_about_two_minutes_out(*expires_unix_secs);
    }
    let mac_code = code_of(&mac_code_state);
    let phone_code = code_of(&phone_code_state);
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
    assert_eq!(
        on_mac[0].kind,
        DeviceKind::Phone,
        "the Mac reports the phone's own kind, from its Config"
    );
    let on_phone = phone.engine.devices();
    assert_eq!(on_phone.len(), 1, "the phone lists the Mac");
    assert_eq!(on_phone[0].name, "Vamana");
    assert_eq!(
        on_phone[0].kind,
        DeviceKind::Mac,
        "the phone reports the Mac's own kind, from its Config"
    );

    // A real file, over a real socket, verified chunk by chunk. The remote
    // path names the phone's one root, "Root".
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let id = mac
        .engine
        .pull(
            on_mac[0].key_hex.clone(),
            "Root/holiday.bin".to_owned(),
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

    let landed = std::fs::read(mac.download_root.join("holiday.bin"))
        .expect("the file should be in the Mac's download folder");
    assert_eq!(landed, bytes, "every byte must match");
    assert!(
        !mac.shared_root.join("holiday.bin").exists(),
        "a pull never writes into a served root"
    );

    let leftovers: Vec<String> = std::fs::read_dir(&mac.download_root)
        .expect("the Mac's download folder should be readable")
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

    // Batch B, item 3, and the C3 fix: the pull dialled the phone, and the
    // phone accepted that connection, so a Wi-Fi success should be on
    // record on both sides, not only the side that dialled. The Mac's own
    // dial already proves this regardless of this machine's own tools.
    // `transport_for_inbound` treats a loopback connection as `Usb` on a
    // machine with no `adb`, the same ambiguity
    // `status_reports_reachability_listen_port_and_adb_presence` below
    // works around, so the phone's side of this only tells the two apart on
    // a machine that has `adb`.
    assert!(
        mac.engine.devices()[0]
            .available_transports
            .contains(&Transport::Wifi),
        "the Mac, which dialled, lists Wifi for the phone"
    );
    if ferry_core::adb::find_adb().is_some() {
        assert!(
            phone.engine.devices()[0]
                .available_transports
                .contains(&Transport::Wifi),
            "the phone, which accepted the connection, lists Wifi for the Mac too"
        );
    }

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

    let on_mac = mac.engine.devices();
    assert_eq!(on_mac.len(), 1, "the Mac lists the phone");
    let phone_key = on_mac[0].key_hex.clone();

    // `Photos` stands in for the first folder a person opens inside the
    // phone's one root, named "Root".
    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("Photos/holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    // The empty path lists every root, not the folder each one holds.
    let top_level = mac
        .engine
        .list(phone_key.clone(), String::new())
        .expect("the roots should list");
    let root_names: Vec<String> = top_level.iter().map(|entry| entry.name.clone()).collect();
    assert_eq!(
        root_names,
        vec!["Root".to_owned()],
        "list(\"\") returns the phone's root names, not what is inside them"
    );

    let entries = mac
        .engine
        .list(phone_key.clone(), "Root/Photos".to_owned())
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
        .list(phone_key, "Root/no-such-folder".to_owned())
        .expect_err("a folder that does not exist cannot be listed");
    assert_eq!(code_of_error(&missing), "OpError::NotFound");

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 2: a folder copy groups its transfers into one batch.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, batch D, item 2, end to end: `pull_folder`
/// lists a folder with a nested subfolder, lands every file at the right
/// relative path, and `batches()` reports the aggregates the contract
/// promises once every file is done. Alongside it: a single `pull` names no
/// batch, and an empty folder makes a batch with zero files, already `Done`.
#[test]
// One long, linear narrative, the same choice `two_engines_pair_and_move_a_file`
// makes: every fact this item promises, checked in the order a folder copy
// actually reaches it.
#[allow(clippy::too_many_lines)]
fn pull_folder_groups_its_transfers_into_one_batch() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Camera")).expect("a folder for the camera roll");
    std::fs::create_dir(phone.shared_root.join("Camera/Sub")).expect("a nested subfolder");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    let bytes_c = sample_bytes();
    std::fs::write(phone.shared_root.join("Camera/a.bin"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/b.bin"), &bytes_b)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/Sub/c.bin"), &bytes_c)
        .expect("the phone's shared folder should accept a nested file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key.clone(), "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_batch = batch_id.clone();
    mac.inbox.wait_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted_batch && b.state == TransferState::Done)
    });

    let batch = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the finished batch should still be listed");
    assert_eq!(batch.files_total, 3, "three files were under the folder");
    assert_eq!(batch.files_done, 3, "every file finished");
    assert_eq!(
        batch.bytes_total,
        (bytes_a.len() + bytes_b.len() + bytes_c.len()) as u64,
        "the byte total is the sum over its transfers"
    );
    assert_eq!(batch.state, TransferState::Done);
    assert_eq!(batch.device_key_hex, phone_key);

    let transfers = mac.engine.transfers();
    let in_batch: Vec<&ferry_runtime::TransferInfo> = transfers
        .iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch.id.as_str()))
        .collect();
    assert_eq!(
        in_batch.len(),
        3,
        "every transfer this folder copy made carries the batch id"
    );

    assert_eq!(
        std::fs::read(mac.download_root.join("Camera/a.bin")).expect("a.bin should have landed"),
        bytes_a
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("Camera/b.bin")).expect("b.bin should have landed"),
        bytes_b
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("Camera/Sub/c.bin"))
            .expect("the nested file should have landed at its relative path"),
        bytes_c
    );

    // A single pull, outside any folder copy, names no batch.
    let single_id = mac
        .engine
        .pull(
            phone_key.clone(),
            "Root/Camera/a.bin".to_owned(),
            "alone.bin".to_owned(),
        )
        .expect("a single pull should be accepted");
    let engine = Arc::clone(&mac.engine);
    let wanted_single = single_id.clone();
    mac.inbox.wait_until("the single pull to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_single && t.state == TransferState::Done)
    });
    let single = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == single_id)
        .expect("the single transfer should still be listed");
    assert_eq!(single.batch_id, None, "a single pull has no batch");

    // An empty folder makes a batch with zero files, already `Done`.
    std::fs::create_dir(phone.shared_root.join("Empty")).expect("an empty folder");
    let empty_batch_id = mac
        .engine
        .pull_folder(phone_key, "Root/Empty".to_owned())
        .expect("an empty folder copy should be accepted");
    let empty_batch = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == empty_batch_id)
        .expect("the empty batch should be listed");
    assert_eq!(empty_batch.files_total, 0);
    assert_eq!(empty_batch.files_done, 0);
    assert_eq!(empty_batch.state, TransferState::Done);
    assert_eq!(
        empty_batch.ended_unix_secs,
        Some(empty_batch.started_unix_secs),
        "a batch with zero files ends when it starts"
    );

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 14: automatic copying, end to end over two real engines.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 14, over the real named-root system
/// `pull_folder`'s own test above already proves: turning the switch on for
/// a device that is already reachable copies its whole `DCIM` folder once,
/// and turning it off then on again, with nothing new on the phone, copies
/// nothing and still records that the run happened.
#[test]
fn turning_on_automatic_copying_copies_the_camera_folder_once() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("DCIM")).expect("a folder for the camera roll");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    std::fs::write(phone.shared_root.join("DCIM/a.jpg"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("DCIM/b.jpg"), &bytes_b)
        .expect("the phone's shared folder should accept a file");

    // Nothing has dialed the phone yet, so the Mac does not yet consider it
    // reachable. A listing is the cheapest real operation that dials it.
    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("listing the phone's roots should succeed");

    let before = mac.engine.auto_copy(phone_key.clone());
    assert!(!before.enabled, "the switch starts off");

    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("the device is paired, so the switch should turn on");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until("the first run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(2)
    });

    let after_first_run = mac.engine.auto_copy(phone_key.clone());
    assert!(after_first_run.enabled);
    assert_eq!(after_first_run.source, "Root/DCIM");
    assert_eq!(
        after_first_run.destination,
        format!("{}/DCIM", mac.download_root.display())
    );
    assert!(after_first_run.last_run_unix_secs.is_some());
    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/a.jpg")).expect("a.jpg should have landed"),
        bytes_a
    );
    assert_eq!(
        std::fs::read(mac.download_root.join("DCIM/b.jpg")).expect("b.jpg should have landed"),
        bytes_b
    );

    // Turning the switch off and back on again is the alternative
    // `docs/engine-contract.md` item 14 names to a second reachability
    // transition, for proving the same files are never copied twice.
    mac.engine
        .set_auto_copy(phone_key.clone(), false)
        .expect("turning the switch off should succeed");
    mac.engine
        .set_auto_copy(phone_key.clone(), true)
        .expect("turning it back on should succeed");

    let engine = Arc::clone(&mac.engine);
    let wanted = phone_key.clone();
    mac.inbox.wait_until("the second run to finish", move || {
        engine.auto_copy(wanted.clone()).last_run_files == Some(0)
    });

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 13: the access log records what each side did.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, batch E, item 13, end to end: a listing and a
/// pull each leave a `Peer` entry on the served side and a matching `This`
/// entry on the calling side, the device filter narrows to one device, and
/// `access_log_changed` fires on both engines.
#[test]
fn access_log_records_a_listing_and_a_pull_on_both_sides() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("Photos/holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let entries = mac
        .engine
        .list(phone_key.clone(), "Root/Photos".to_owned())
        .expect("the folder should list");
    assert_eq!(entries.len(), 1, "one file sits under Photos");

    let id = mac
        .engine
        .pull(
            phone_key.clone(),
            "Root/Photos/holiday.bin".to_owned(),
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

    // The calling side ("This"): both calls finalise their own entry at
    // once, so nothing here needs to wait.
    let mac_log = mac.engine.access_log(None, 100);
    let this_list = mac_log
        .iter()
        .find(|e| e.actor == Actor::This && e.verb == AccessVerb::List)
        .expect("the calling side should record its own listing");
    assert_eq!(this_list.path, "Root/Photos");
    assert_eq!(this_list.entries, Some(1));

    let this_read = mac_log
        .iter()
        .find(|e| e.actor == Actor::This && e.verb == AccessVerb::Read)
        .expect("the calling side should record its own pull");
    assert_eq!(this_read.path, "Root/Photos/holiday.bin");
    assert_eq!(this_read.bytes, Some(bytes.len() as u64));

    assert!(
        mac.inbox.lock().access_log_ticks > 0,
        "access_log_changed should have fired on the calling side"
    );

    // The served side ("Peer"): the entries finalise once the served
    // connection ends, which happens on the phone's own serving thread, so
    // this polls instead of assuming it has already happened.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record what it served", move || {
            let log = phone_engine.access_log(None, 100);
            log.iter()
                .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::List)
                && log
                    .iter()
                    .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        });
    let phone_log = phone.engine.access_log(None, 100);
    let peer_list = phone_log
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::List)
        .expect("the served side should record the listing");
    assert_eq!(peer_list.entries, Some(1));
    let peer_read = phone_log
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .expect("the served side should record the read");
    assert_eq!(peer_read.bytes, Some(bytes.len() as u64));

    assert!(
        phone.inbox.lock().access_log_ticks > 0,
        "access_log_changed should have fired on the served side"
    );

    // The device filter: an unrelated key hex sees nothing, the real one
    // sees what was just recorded.
    let stranger = "ff".repeat(32);
    assert!(mac.engine.access_log(Some(stranger), 100).is_empty());
    assert!(!mac.engine.access_log(Some(phone_key), 100).is_empty());

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Finding E5: a folder copy logs itself once, not once per file.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 13, "Rolling up": a file copied as part
/// of a folder copy logs nothing of its own on the calling side, because the
/// folder's own entry, with its file count and byte total, already covers
/// it. The served side knows nothing of batches, so it still logs one entry
/// per file, exactly as an ordinary pull would.
#[test]
fn pull_folder_logs_the_folder_once_not_once_per_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Camera")).expect("a folder for the camera roll");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    std::fs::write(phone.shared_root.join("Camera/a.bin"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/b.bin"), &bytes_b)
        .expect("the phone's shared folder should accept a file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key.clone(), "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_batch = batch_id.clone();
    mac.inbox.wait_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted_batch && b.state == TransferState::Done)
    });

    // The calling side: the folder's own Read entry is the only one, and it
    // is not one of the two files.
    let mac_log = mac.engine.access_log(None, 100);
    let this_reads: Vec<_> = mac_log
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Read)
        .collect();
    assert_eq!(
        this_reads.len(),
        1,
        "the folder entry is the only Read the calling side logs"
    );
    assert_eq!(this_reads[0].path, "Root/Camera");
    assert_eq!(this_reads[0].files, Some(2));
    assert_eq!(
        this_reads[0].bytes,
        Some((bytes_a.len() + bytes_b.len()) as u64),
        "the folder entry's byte total covers both files"
    );

    // The served side does not know about batches: it still logs one Read
    // per file, the phone's own serving thread finalising each once its
    // connection ends.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record both files it served", move || {
            phone_engine
                .access_log(None, 100)
                .iter()
                .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
                .count()
                >= 2
        });
    let phone_log = phone.engine.access_log(None, 100);
    let peer_reads: Vec<_> = phone_log
        .iter()
        .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .collect();
    assert_eq!(
        peer_reads.len(),
        2,
        "one entry per file on the served side, unlike the calling side"
    );
    assert!(peer_reads.iter().any(|e| e.path == "Root/Camera/a.bin"));
    assert!(peer_reads.iter().any(|e| e.path == "Root/Camera/b.bin"));

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 15: roots reach an open connection without a reconnect.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, batch C, item 15: a root change must reach an
/// already-connected peer on its very next operation, with no reconnect.
/// `Engine::list` always dials fresh, so this drives one connection by hand,
/// the same way `engine_paths.rs` does, to keep it open across the change.
#[test]
fn a_root_change_reaches_an_open_connection_without_a_reconnect() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    // Only pairing itself is needed here: this test drives its own raw
    // connection rather than `mac.engine.list`.
    let _phone_key = pair(&mac, &phone);

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&mac.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Vamana", CoreDeviceKind::Mac)
        .expect("the name exchange should run");
    let mut client = Client::new(stream);

    let root_path = RemotePath::parse("").expect("the empty path is valid");
    let (before, _) = client
        .list(&root_path, 0)
        .expect("the roots should list on the freshly opened connection");
    assert_eq!(
        before.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Root".to_owned()],
        "before the change, only the configured root is listed"
    );

    let renamed_root = tempfile::tempdir().expect("a temporary folder for the renamed root");
    phone
        .engine
        .set_roots(vec![Root {
            name: "Renamed".to_owned(),
            path: renamed_root.path().to_string_lossy().into_owned(),
            writable: true,
        }])
        .expect("set_roots should accept a fresh, valid root");

    // The very next operation, on the very same connection: no reconnect.
    let (after, _) = client
        .list(&root_path, 0)
        .expect("the roots should list again, on the same connection");
    assert_eq!(
        after.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Renamed".to_owned()],
        "the already-open connection sees the new root without reconnecting"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// docs/engine-contract.md item 16a: a manifest request answers with the
/// same manifest `ManifestBuilder` gives over the file's own bytes, once it
/// has travelled a real, paired connection.
#[test]
fn a_manifest_request_crosses_the_wire() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let _phone_key = pair(&mac, &phone);

    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("holiday.bin"), &bytes)
        .expect("the file should write to the shared root");

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&mac.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Vamana", CoreDeviceKind::Mac)
        .expect("the name exchange should run");
    let mut client = Client::new(stream);

    let path = RemotePath::parse("Root/holiday.bin").expect("a valid path");
    let manifest = client
        .manifest(&path)
        .expect("the peer should answer a manifest request");
    let expected = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    assert_eq!(
        manifest, expected,
        "the served manifest must match the file's own bytes"
    );

    let root_path = RemotePath::parse("Root").expect("a valid path");
    let dir_error = client
        .manifest(&root_path)
        .expect_err("a directory has no manifest");
    assert!(
        matches!(dir_error, RpcError::Remote(OpError::IsADirectory)),
        "expected IsADirectory, got {dir_error:?}"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// Batch B and C audit, C6: `set_roots` refuses a bad set, and the roots it
/// already had keep serving.
#[test]
fn a_bad_set_roots_call_is_refused_and_the_old_roots_keep_serving() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    let outer = tempfile::tempdir().expect("a temporary folder for the outer root");
    let inner_path = outer.path().join("inner");
    std::fs::create_dir(&inner_path).expect("a nested folder for the inner root");

    let error = phone
        .engine
        .set_roots(vec![
            Root {
                name: "Outer".to_owned(),
                path: outer.path().to_string_lossy().into_owned(),
                writable: true,
            },
            Root {
                name: "Inner".to_owned(),
                path: inner_path.to_string_lossy().into_owned(),
                writable: true,
            },
        ])
        .expect_err("a root nested inside another root should be refused");
    assert_eq!(
        code_of_error(&error),
        "RootsError::RootOverlaps",
        "the RootsError code crosses the boundary"
    );

    let entries = mac
        .engine
        .list(phone_key, String::new())
        .expect("the old roots should still serve");
    assert_eq!(
        entries.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Root".to_owned()],
        "a refused set_roots call leaves the previous roots serving"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// Pair `mac` with `phone` and return the phone's key hex, as `mac` names
/// it. Shared by tests that need two paired engines but do not otherwise
/// exercise the pairing screens.
fn pair(mac: &Side, phone: &Side) -> String {
    phone.engine.set_reachable(true);
    phone.engine.start_pairing();
    mac.engine.start_pairing();

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

    let root_source = side
        .engine
        .pull("00".repeat(32), String::new(), "a.bin".to_owned())
        .expect_err("the shared root has no single file to pull");
    assert_eq!(code_of_error(&root_source), "PathError::Empty");

    let unpaired = side
        .engine
        .pull("00".repeat(32), "a.bin".to_owned(), "a.bin".to_owned())
        .expect_err("an unpaired device cannot be pulled from");
    assert_eq!(code_of_error(&unpaired), "Runtime::NotPaired");

    side.engine.stop();
}

#[test]
fn a_reachable_engine_has_a_four_character_short_code() {
    let phone = build("Pixel 3 XL");

    phone.engine.set_reachable(true);
    let code = phone
        .engine
        .short_code()
        .expect("a reachable engine should show its own short code");
    assert_eq!(code.chars().count(), 4);

    phone.engine.set_reachable(false);
    assert_eq!(
        phone.engine.short_code(),
        None,
        "an unreachable engine shows no short code"
    );

    phone.engine.stop();
}

#[test]
fn status_reports_reachability_listen_port_and_adb_presence() {
    let phone = build("Pixel 3 XL");

    let before = phone.engine.status();
    assert!(!before.reachable, "reachability starts off");
    assert_ne!(before.listen_port, 0, "a started engine has bound a port");
    assert_eq!(
        before.listen_port,
        phone
            .engine
            .listen_addr()
            .expect("the engine has started")
            .port(),
        "status reports the same port the engine bound"
    );
    assert_eq!(
        before.adb_present,
        ferry_core::adb::find_adb().is_some(),
        "status reports whether this machine has adb, same as the engine found at start"
    );

    phone.engine.set_reachable(true);
    assert!(
        phone.engine.status().reachable,
        "status follows set_reachable"
    );

    phone.engine.stop();
}

#[test]
fn a_bad_config_is_refused_before_anything_starts() {
    let data = tempfile::tempdir().expect("a temporary folder");
    let shared = tempfile::tempdir().expect("a temporary folder");
    let download = tempfile::tempdir().expect("a temporary folder");
    let inbox = Arc::new(Inbox::default());
    let make = |name: String, key: KeyPair| {
        Engine::new(
            Config {
                data_dir: data.path().to_string_lossy().into_owned(),
                shared_roots: vec![Root {
                    name: "Root".to_owned(),
                    path: shared.path().to_string_lossy().into_owned(),
                    writable: true,
                }],
                download_dir: download.path().to_string_lossy().into_owned(),
                display_name: name,
                listen_port: 0,
                key,
                kind: DeviceKind::Mac,
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

// ---------------------------------------------------------------------------
// Item 5: push.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 5, the happy path: a pushed file lands on
/// the peer under the right root, with the right bytes and the right
/// modified time, the row shows `Direction::Push` and ends `Done`, the
/// peer's access log shows the writes, and the sender's shows one `Write`
/// entry for the whole file.
#[test]
fn a_pushed_file_lands_on_the_peer_and_the_row_and_logs_show_it() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    // The file being pushed lives outside every served root, the way a real
    // file the person picks in a panel would.
    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("holiday.bin");
    let bytes = sample_bytes();
    std::fs::write(&local_path, &bytes).expect("the local file should write");
    let old_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&local_path)
        .expect("the local file should reopen")
        .set_modified(old_mtime)
        .expect("the local file's modified time should be settable");

    let id = mac
        .engine
        .push(
            phone_key.clone(),
            local_path.to_string_lossy().into_owned(),
            "Root/holiday.bin".to_owned(),
        )
        .expect("the push should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_id = id.clone();
    mac.inbox.wait_until("the push to finish", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_id && t.state == TransferState::Done)
    });

    let row = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the finished push should still be listed");
    assert_eq!(row.direction, Direction::Push, "a push is Direction::Push");
    assert_eq!(row.state, TransferState::Done);

    let landed_path = phone.shared_root.join("holiday.bin");
    let landed = std::fs::read(&landed_path).expect("the file should be under the phone's root");
    assert_eq!(landed, bytes, "every byte must match");

    let landed_mtime = std::fs::metadata(&landed_path)
        .expect("the landed file should have metadata")
        .modified()
        .expect("the platform should report a modified time")
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let wanted_mtime = old_mtime
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    assert_eq!(
        landed_mtime, wanted_mtime,
        "the pushed file keeps the local file's modified time"
    );

    let leftovers: Vec<String> = std::fs::read_dir(&phone.shared_root)
        .expect("the phone's shared folder should be readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".ferry-part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no partial file should be left behind, found {leftovers:?}"
    );

    // The peer's own access log shows the writes it served, as actor Peer.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record the writes it served", move || {
            phone_engine
                .access_log(None, 100)
                .iter()
                .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Write)
        });

    // The sender's own access log: one Write entry for the one pushed file.
    let mac_log = mac.engine.access_log(None, 100);
    let this_writes: Vec<_> = mac_log
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Write)
        .collect();
    assert_eq!(
        this_writes.len(),
        1,
        "one Write entry for the one pushed file"
    );
    assert_eq!(this_writes[0].path, "Root/holiday.bin");
    assert_eq!(this_writes[0].bytes, Some(bytes.len() as u64));

    mac.engine.stop();
    phone.engine.stop();
}

/// A push into a root the peer marked not writable fails with the same
/// `PermissionDenied` a read-only root already refuses everything else with.
#[test]
fn a_push_into_a_read_only_root_fails_with_permission_denied() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    phone
        .engine
        .set_roots(vec![Root {
            name: "Root".to_owned(),
            path: phone.shared_root.to_string_lossy().into_owned(),
            writable: false,
        }])
        .expect("the phone should accept its own root marked read-only");

    let source = tempfile::tempdir().expect("a folder for the file being pushed");
    let local_path = source.path().join("holiday.bin");
    std::fs::write(&local_path, sample_bytes()).expect("the local file should write");

    let id = mac
        .engine
        .push(
            phone_key,
            local_path.to_string_lossy().into_owned(),
            "Root/holiday.bin".to_owned(),
        )
        .expect("the push should be accepted; the refusal is the peer's, not the call's");

    let engine = Arc::clone(&mac.engine);
    let wanted_id = id.clone();
    mac.inbox.wait_until("the push to fail", move || {
        engine
            .transfers()
            .iter()
            .any(|t| t.id == wanted_id && t.state == TransferState::Failed)
    });

    let row = mac
        .engine
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("the failed push should still be listed");
    let error = row.error.expect("a failed row carries an error");
    assert_eq!(code_of_error(&error), "OpError::PermissionDenied");
    assert!(
        !phone.shared_root.join("holiday.bin").exists(),
        "nothing lands on a root that refused the write"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// `push_files` makes one batch and lands every file, the push mirror of
/// `pull_folder_groups_its_transfers_into_one_batch`.
#[test]
fn push_files_makes_one_batch_and_lands_every_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Uploads"))
        .expect("a folder on the phone to push into");

    let source = tempfile::tempdir().expect("a folder for the files being pushed");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    let bytes_c = sample_bytes();
    let path_a = source.path().join("a.bin");
    let path_b = source.path().join("b.bin");
    let path_c = source.path().join("c.bin");
    std::fs::write(&path_a, &bytes_a).expect("a.bin should write");
    std::fs::write(&path_b, &bytes_b).expect("b.bin should write");
    std::fs::write(&path_c, &bytes_c).expect("c.bin should write");

    let batch_id = mac
        .engine
        .push_files(
            phone_key,
            vec![
                path_a.to_string_lossy().into_owned(),
                path_b.to_string_lossy().into_owned(),
                path_c.to_string_lossy().into_owned(),
            ],
            "Root/Uploads".to_owned(),
        )
        .expect("push_files should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_batch = batch_id.clone();
    mac.inbox.wait_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted_batch && b.state == TransferState::Done)
    });

    let batch = mac
        .engine
        .batches()
        .into_iter()
        .find(|b| b.id == batch_id)
        .expect("the finished batch should still be listed");
    assert_eq!(batch.files_total, 3, "three files were pushed");
    assert_eq!(batch.files_done, 3, "every file finished");
    assert_eq!(batch.direction, Direction::Push);
    assert_eq!(
        batch.bytes_total,
        (bytes_a.len() + bytes_b.len() + bytes_c.len()) as u64,
        "the byte total is the sum over its transfers"
    );

    assert_eq!(
        std::fs::read(phone.shared_root.join("Uploads/a.bin")).expect("a.bin should have landed"),
        bytes_a
    );
    assert_eq!(
        std::fs::read(phone.shared_root.join("Uploads/b.bin")).expect("b.bin should have landed"),
        bytes_b
    );
    assert_eq!(
        std::fs::read(phone.shared_root.join("Uploads/c.bin")).expect("c.bin should have landed"),
        bytes_c
    );

    let transfers = mac.engine.transfers();
    let in_batch: Vec<&ferry_runtime::TransferInfo> = transfers
        .iter()
        .filter(|t| t.batch_id.as_deref() == Some(batch.id.as_str()))
        .collect();
    assert_eq!(
        in_batch.len(),
        3,
        "every transfer this push_files call made carries the batch id"
    );

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 13: the access log records what each side did.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, batch E, item 13, end to end: a listing and a
/// pull each leave a `Peer` entry on the served side and a matching `This`
/// entry on the calling side, the device filter narrows to one device, and
/// `access_log_changed` fires on both engines.
#[test]
fn access_log_records_a_listing_and_a_pull_on_both_sides() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Photos"))
        .expect("the phone's shared folder should accept a new folder");
    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("Photos/holiday.bin"), &bytes)
        .expect("the phone's shared folder should accept a file");

    let entries = mac
        .engine
        .list(phone_key.clone(), "Root/Photos".to_owned())
        .expect("the folder should list");
    assert_eq!(entries.len(), 1, "one file sits under Photos");

    let id = mac
        .engine
        .pull(
            phone_key.clone(),
            "Root/Photos/holiday.bin".to_owned(),
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

    // The calling side ("This"): both calls finalise their own entry at
    // once, so nothing here needs to wait.
    let mac_log = mac.engine.access_log(None, 100);
    let this_list = mac_log
        .iter()
        .find(|e| e.actor == Actor::This && e.verb == AccessVerb::List)
        .expect("the calling side should record its own listing");
    assert_eq!(this_list.path, "Root/Photos");
    assert_eq!(this_list.entries, Some(1));

    let this_read = mac_log
        .iter()
        .find(|e| e.actor == Actor::This && e.verb == AccessVerb::Read)
        .expect("the calling side should record its own pull");
    assert_eq!(this_read.path, "Root/Photos/holiday.bin");
    assert_eq!(this_read.bytes, Some(bytes.len() as u64));

    assert!(
        mac.inbox.lock().access_log_ticks > 0,
        "access_log_changed should have fired on the calling side"
    );

    // The served side ("Peer"): the entries finalise once the served
    // connection ends, which happens on the phone's own serving thread, so
    // this polls instead of assuming it has already happened.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record what it served", move || {
            let log = phone_engine.access_log(None, 100);
            log.iter()
                .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::List)
                && log
                    .iter()
                    .any(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        });
    let phone_log = phone.engine.access_log(None, 100);
    let peer_list = phone_log
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::List)
        .expect("the served side should record the listing");
    assert_eq!(peer_list.entries, Some(1));
    let peer_read = phone_log
        .iter()
        .find(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .expect("the served side should record the read");
    assert_eq!(peer_read.bytes, Some(bytes.len() as u64));

    assert!(
        phone.inbox.lock().access_log_ticks > 0,
        "access_log_changed should have fired on the served side"
    );

    // The device filter: an unrelated key hex sees nothing, the real one
    // sees what was just recorded.
    let stranger = "ff".repeat(32);
    assert!(mac.engine.access_log(Some(stranger), 100).is_empty());
    assert!(!mac.engine.access_log(Some(phone_key), 100).is_empty());

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Finding E5: a folder copy logs itself once, not once per file.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 13, "Rolling up": a file copied as part
/// of a folder copy logs nothing of its own on the calling side, because the
/// folder's own entry, with its file count and byte total, already covers
/// it. The served side knows nothing of batches, so it still logs one entry
/// per file, exactly as an ordinary pull would.
#[test]
fn pull_folder_logs_the_folder_once_not_once_per_file() {
    let phone = build("Pixel 3 XL");
    let mac = build("Vamana");
    let phone_key = pair(&mac, &phone);

    std::fs::create_dir(phone.shared_root.join("Camera")).expect("a folder for the camera roll");
    let bytes_a = sample_bytes();
    let bytes_b = sample_bytes();
    std::fs::write(phone.shared_root.join("Camera/a.bin"), &bytes_a)
        .expect("the phone's shared folder should accept a file");
    std::fs::write(phone.shared_root.join("Camera/b.bin"), &bytes_b)
        .expect("the phone's shared folder should accept a file");

    let batch_id = mac
        .engine
        .pull_folder(phone_key.clone(), "Root/Camera".to_owned())
        .expect("the folder copy should be accepted");

    let engine = Arc::clone(&mac.engine);
    let wanted_batch = batch_id.clone();
    mac.inbox.wait_until("the batch to finish", move || {
        engine
            .batches()
            .iter()
            .any(|b| b.id == wanted_batch && b.state == TransferState::Done)
    });

    // The calling side: the folder's own Read entry is the only one, and it
    // is not one of the two files.
    let mac_log = mac.engine.access_log(None, 100);
    let this_reads: Vec<_> = mac_log
        .iter()
        .filter(|e| e.actor == Actor::This && e.verb == AccessVerb::Read)
        .collect();
    assert_eq!(
        this_reads.len(),
        1,
        "the folder entry is the only Read the calling side logs"
    );
    assert_eq!(this_reads[0].path, "Root/Camera");
    assert_eq!(this_reads[0].files, Some(2));
    assert_eq!(
        this_reads[0].bytes,
        Some((bytes_a.len() + bytes_b.len()) as u64),
        "the folder entry's byte total covers both files"
    );

    // The served side does not know about batches: it still logs one Read
    // per file, the phone's own serving thread finalising each once its
    // connection ends.
    let phone_engine = Arc::clone(&phone.engine);
    phone
        .inbox
        .wait_until("the phone to record both files it served", move || {
            phone_engine
                .access_log(None, 100)
                .iter()
                .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
                .count()
                >= 2
        });
    let phone_log = phone.engine.access_log(None, 100);
    let peer_reads: Vec<_> = phone_log
        .iter()
        .filter(|e| e.actor == Actor::Peer && e.verb == AccessVerb::Read)
        .collect();
    assert_eq!(
        peer_reads.len(),
        2,
        "one entry per file on the served side, unlike the calling side"
    );
    assert!(peer_reads.iter().any(|e| e.path == "Root/Camera/a.bin"));
    assert!(peer_reads.iter().any(|e| e.path == "Root/Camera/b.bin"));

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 15: roots reach an open connection without a reconnect.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, batch C, item 15: a root change must reach an
/// already-connected peer on its very next operation, with no reconnect.
/// `Engine::list` always dials fresh, so this drives one connection by hand,
/// the same way `engine_paths.rs` does, to keep it open across the change.
#[test]
fn a_root_change_reaches_an_open_connection_without_a_reconnect() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    // Only pairing itself is needed here: this test drives its own raw
    // connection rather than `mac.engine.list`.
    let _phone_key = pair(&mac, &phone);

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&mac.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Vamana", CoreDeviceKind::Mac)
        .expect("the name exchange should run");
    let mut client = Client::new(stream);

    let root_path = RemotePath::parse("").expect("the empty path is valid");
    let (before, _) = client
        .list(&root_path, 0)
        .expect("the roots should list on the freshly opened connection");
    assert_eq!(
        before.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Root".to_owned()],
        "before the change, only the configured root is listed"
    );

    let renamed_root = tempfile::tempdir().expect("a temporary folder for the renamed root");
    phone
        .engine
        .set_roots(vec![Root {
            name: "Renamed".to_owned(),
            path: renamed_root.path().to_string_lossy().into_owned(),
            writable: true,
        }])
        .expect("set_roots should accept a fresh, valid root");

    // The very next operation, on the very same connection: no reconnect.
    let (after, _) = client
        .list(&root_path, 0)
        .expect("the roots should list again, on the same connection");
    assert_eq!(
        after.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Renamed".to_owned()],
        "the already-open connection sees the new root without reconnecting"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// docs/engine-contract.md item 16a: a manifest request answers with the
/// same manifest `ManifestBuilder` gives over the file's own bytes, once it
/// has travelled a real, paired connection.
#[test]
fn a_manifest_request_crosses_the_wire() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let _phone_key = pair(&mac, &phone);

    let bytes = sample_bytes();
    std::fs::write(phone.shared_root.join("holiday.bin"), &bytes)
        .expect("the file should write to the shared root");

    let connection = tcp::connect(
        loopback_addr(&phone),
        &static_key(&mac.key),
        &public_key(&phone.key),
    )
    .expect("a paired peer should be able to connect");
    let mut stream = connection.stream;
    exchange_hello(&mut stream, "Vamana", CoreDeviceKind::Mac)
        .expect("the name exchange should run");
    let mut client = Client::new(stream);

    let path = RemotePath::parse("Root/holiday.bin").expect("a valid path");
    let manifest = client
        .manifest(&path)
        .expect("the peer should answer a manifest request");
    let expected = manifest_from_bytes(&bytes, ChunkSize::one_mebibyte());
    assert_eq!(
        manifest, expected,
        "the served manifest must match the file's own bytes"
    );

    let root_path = RemotePath::parse("Root").expect("a valid path");
    let dir_error = client
        .manifest(&root_path)
        .expect_err("a directory has no manifest");
    assert!(
        matches!(dir_error, RpcError::Remote(OpError::IsADirectory)),
        "expected IsADirectory, got {dir_error:?}"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// Batch B and C audit, C6: `set_roots` refuses a bad set, and the roots it
/// already had keep serving.
#[test]
fn a_bad_set_roots_call_is_refused_and_the_old_roots_keep_serving() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    let outer = tempfile::tempdir().expect("a temporary folder for the outer root");
    let inner_path = outer.path().join("inner");
    std::fs::create_dir(&inner_path).expect("a nested folder for the inner root");

    let error = phone
        .engine
        .set_roots(vec![
            Root {
                name: "Outer".to_owned(),
                path: outer.path().to_string_lossy().into_owned(),
                writable: true,
            },
            Root {
                name: "Inner".to_owned(),
                path: inner_path.to_string_lossy().into_owned(),
                writable: true,
            },
        ])
        .expect_err("a root nested inside another root should be refused");
    assert_eq!(
        code_of_error(&error),
        "RootsError::RootOverlaps",
        "the RootsError code crosses the boundary"
    );

    let entries = mac
        .engine
        .list(phone_key, String::new())
        .expect("the old roots should still serve");
    assert_eq!(
        entries.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["Root".to_owned()],
        "a refused set_roots call leaves the previous roots serving"
    );

    mac.engine.stop();
    phone.engine.stop();
}

/// Pair `mac` with `phone` and return the phone's key hex, as `mac` names
/// it. Shared by tests that need two paired engines but do not otherwise
/// exercise the pairing screens.
fn pair(mac: &Side, phone: &Side) -> String {
    phone.engine.set_reachable(true);
    phone.engine.start_pairing();
    mac.engine.start_pairing();

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

    let root_source = side
        .engine
        .pull("00".repeat(32), String::new(), "a.bin".to_owned())
        .expect_err("the shared root has no single file to pull");
    assert_eq!(code_of_error(&root_source), "PathError::Empty");

    let unpaired = side
        .engine
        .pull("00".repeat(32), "a.bin".to_owned(), "a.bin".to_owned())
        .expect_err("an unpaired device cannot be pulled from");
    assert_eq!(code_of_error(&unpaired), "Runtime::NotPaired");

    side.engine.stop();
}

#[test]
fn a_reachable_engine_has_a_four_character_short_code() {
    let phone = build("Pixel 3 XL");

    phone.engine.set_reachable(true);
    let code = phone
        .engine
        .short_code()
        .expect("a reachable engine should show its own short code");
    assert_eq!(code.chars().count(), 4);

    phone.engine.set_reachable(false);
    assert_eq!(
        phone.engine.short_code(),
        None,
        "an unreachable engine shows no short code"
    );

    phone.engine.stop();
}

#[test]
fn status_reports_reachability_listen_port_and_adb_presence() {
    let phone = build("Pixel 3 XL");

    let before = phone.engine.status();
    assert!(!before.reachable, "reachability starts off");
    assert_ne!(before.listen_port, 0, "a started engine has bound a port");
    assert_eq!(
        before.listen_port,
        phone
            .engine
            .listen_addr()
            .expect("the engine has started")
            .port(),
        "status reports the same port the engine bound"
    );
    assert_eq!(
        before.adb_present,
        ferry_core::adb::find_adb().is_some(),
        "status reports whether this machine has adb, same as the engine found at start"
    );

    phone.engine.set_reachable(true);
    assert!(
        phone.engine.status().reachable,
        "status follows set_reachable"
    );

    phone.engine.stop();
}

#[test]
fn a_bad_config_is_refused_before_anything_starts() {
    let data = tempfile::tempdir().expect("a temporary folder");
    let shared = tempfile::tempdir().expect("a temporary folder");
    let download = tempfile::tempdir().expect("a temporary folder");
    let inbox = Arc::new(Inbox::default());
    let make = |name: String, key: KeyPair| {
        Engine::new(
            Config {
                data_dir: data.path().to_string_lossy().into_owned(),
                shared_roots: vec![Root {
                    name: "Root".to_owned(),
                    path: shared.path().to_string_lossy().into_owned(),
                    writable: true,
                }],
                download_dir: download.path().to_string_lossy().into_owned(),
                display_name: name,
                listen_port: 0,
                key,
                kind: DeviceKind::Mac,
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
