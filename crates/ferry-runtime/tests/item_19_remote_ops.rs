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
//! item, never appended to a shared one. The one test here that speaks HTTP
//! to a running bridge uses the shared client in `tests/common/mod.rs`
//! instead of copying it again.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_core::limits::MAX_WRITE_LEN;
use ferry_runtime::{
    AccessEntry, AccessVerb, Actor, Config, DeviceKind, Engine, EngineListener, EntryKind, KeyPair,
    PairingMethod, PairingState, Root, generate_key,
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

/// The code an error carries, or a panic saying it had none.
fn code_of_error(error: &ferry_runtime::FerryError) -> String {
    let ferry_runtime::FerryError::Failed { code, .. } = error;
    code.clone()
}

/// This side's own entry for `verb`, or a panic naming what is missing.
///
/// Every call below finalises its own entry before it returns, the way
/// `record_this` does, so nothing here has to wait for it.
fn this_entry(log: &[AccessEntry], verb: AccessVerb) -> &AccessEntry {
    log.iter()
        .find(|entry| entry.actor == Actor::This && entry.verb == verb)
        .unwrap_or_else(|| panic!("the calling side should have recorded one {verb:?}"))
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

// ---------------------------------------------------------------------------
// Item 19: the seven file operations, and what each one logs.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 19: "One test walks stat, write at zero,
/// read back, truncate, rename, mkdir, and delete against the peer and
/// checks the phone-side access log holds one entry per verb with the
/// bytes."
///
/// The Mac stands in for the phone's `DocumentsProvider` here. It is the
/// calling side, so the entries it records are its own, with actor `This`.
#[test]
// One long, linear walk is the point: every verb the provider needs, in the
// order a person editing a file actually reaches them.
#[allow(clippy::too_many_lines)]
fn the_seven_file_operations_work_against_a_peer_and_log_themselves() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    let body = b"the phone's file picker wrote this".to_vec();
    let body_len = u32::try_from(body.len()).expect("the sample body fits in a u32");

    // `mkdir`: one new folder inside the phone's one root.
    mac.engine
        .mkdir(phone_key.clone(), "Root/Notes".to_owned())
        .expect("a folder should be creatable inside a writable root");
    assert!(
        phone.shared_root.join("Notes").is_dir(),
        "the folder must exist on the phone's disk"
    );

    // `write_at` at offset zero creates the file, as the contract says:
    // "Creates the file when it does not exist."
    mac.engine
        .write_at(
            phone_key.clone(),
            "Root/Notes/draft.txt".to_owned(),
            0,
            body.clone(),
        )
        .expect("a write at zero should create the file");
    assert_eq!(
        std::fs::read(phone.shared_root.join("Notes/draft.txt"))
            .expect("the written file should be on the phone's disk"),
        body,
        "the bytes on disk must be the bytes sent"
    );

    // `stat`: the file the write just made.
    let stat = mac
        .engine
        .stat(phone_key.clone(), "Root/Notes/draft.txt".to_owned())
        .expect("the new file should stat");
    assert_eq!(stat.name, "draft.txt");
    assert_eq!(stat.kind, EntryKind::File);
    assert_eq!(
        stat.size,
        body.len() as u64,
        "stat reports what was written"
    );

    // `read_at`: the same bytes back.
    let read = mac
        .engine
        .read_at(
            phone_key.clone(),
            "Root/Notes/draft.txt".to_owned(),
            0,
            body_len,
        )
        .expect("the file should read back");
    assert_eq!(read, body, "a read at zero returns what was written");

    // An ask longer than the file is clamped, not refused. A short read is
    // an ordinary read result.
    let short = mac
        .engine
        .read_at(
            phone_key.clone(),
            "Root/Notes/draft.txt".to_owned(),
            0,
            MAX_WRITE_LEN,
        )
        .expect("an ask longer than the file is a short read, not an error");
    assert_eq!(short, body);

    // `truncate`: cut the file to its first eight bytes.
    mac.engine
        .truncate(phone_key.clone(), "Root/Notes/draft.txt".to_owned(), 8)
        .expect("the file should truncate");
    assert_eq!(
        std::fs::metadata(phone.shared_root.join("Notes/draft.txt"))
            .expect("the truncated file should still be there")
            .len(),
        8,
        "truncate sets the length on the phone's disk"
    );

    // `rename`: within the one root, as the bridge allows.
    mac.engine
        .rename(
            phone_key.clone(),
            "Root/Notes/draft.txt".to_owned(),
            "Root/Notes/final.txt".to_owned(),
        )
        .expect("a rename inside one root should be allowed");
    assert!(
        phone.shared_root.join("Notes/final.txt").is_file(),
        "the file must be at its new name"
    );
    assert!(
        !phone.shared_root.join("Notes/draft.txt").exists(),
        "the file must be gone from its old name"
    );

    // `delete`: the file, then the folder it leaves empty. The wire has no
    // recursive delete, so the order matters.
    mac.engine
        .delete(phone_key.clone(), "Root/Notes/final.txt".to_owned())
        .expect("the file should delete");
    mac.engine
        .delete(phone_key.clone(), "Root/Notes".to_owned())
        .expect("the folder should delete once it is empty");
    assert!(
        !phone.shared_root.join("Notes").exists(),
        "the folder must be gone from the phone's disk"
    );

    // The access log on this side: one entry per verb, with the bytes for a
    // read and a write, as the contract asks.
    let log = mac.engine.access_log(Some(phone_key), 100);

    let stat_entry = this_entry(&log, AccessVerb::Stat);
    assert_eq!(stat_entry.path, "Root/Notes/draft.txt");
    assert_eq!(stat_entry.bytes, None, "a stat moves no bytes");

    let write_entry = this_entry(&log, AccessVerb::Write);
    assert_eq!(write_entry.path, "Root/Notes/draft.txt");
    assert_eq!(
        write_entry.bytes,
        Some(body.len() as u64),
        "a write records the bytes the peer accepted"
    );

    let read_entry = this_entry(&log, AccessVerb::Read);
    assert_eq!(read_entry.path, "Root/Notes/draft.txt");
    assert_eq!(
        read_entry.bytes,
        Some(body.len() as u64),
        "a read records the bytes it returned, not the bytes it asked for"
    );

    let truncate_entry = this_entry(&log, AccessVerb::Truncate);
    assert_eq!(truncate_entry.path, "Root/Notes/draft.txt");
    assert_eq!(truncate_entry.bytes, None);

    let rename_entry = this_entry(&log, AccessVerb::Rename);
    assert_eq!(
        rename_entry.path, "Root/Notes/final.txt",
        "a rename logs where the file ended up, as the serving side does"
    );

    let mkdir_entry = this_entry(&log, AccessVerb::Mkdir);
    assert_eq!(mkdir_entry.path, "Root/Notes");

    let deletes = log
        .iter()
        .filter(|entry| entry.actor == Actor::This && entry.verb == AccessVerb::Delete)
        .count();
    assert_eq!(deletes, 2, "one entry per delete, the file then the folder");

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Item 19: a write over one mebibyte.
// ---------------------------------------------------------------------------

/// `docs/engine-contract.md`, item 19: "One test proves a write over 1 MiB
/// is refused with the row and writes nothing."
///
/// The buffer is built in memory once, one byte over the limit. The refusal
/// happens before the pool is touched, so nothing reaches the wire
/// (`docs/agent-runs.md`, rule 8: ask for the cheap test).
#[test]
fn a_write_over_one_mebibyte_is_refused_and_writes_nothing() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    let one_byte_over = vec![7u8; MAX_WRITE_LEN as usize + 1];
    let error = mac
        .engine
        .write_at(
            phone_key.clone(),
            "Root/too-big.bin".to_owned(),
            0,
            one_byte_over,
        )
        .expect_err("a write of more than one mebibyte should be refused");
    assert_eq!(code_of_error(&error), "Runtime::WriteTooLarge");
    assert!(
        !phone.shared_root.join("too-big.bin").exists(),
        "a refused write must leave no file behind"
    );

    // Exactly one mebibyte is accepted, so the refusal is the size and not
    // the call itself.
    let at_the_limit = vec![9u8; MAX_WRITE_LEN as usize];
    mac.engine
        .write_at(phone_key, "Root/just-fits.bin".to_owned(), 0, at_the_limit)
        .expect("a write of exactly one mebibyte should be accepted");
    assert_eq!(
        std::fs::metadata(phone.shared_root.join("just-fits.bin"))
            .expect("the accepted write should be on the phone's disk")
            .len(),
        u64::from(MAX_WRITE_LEN),
        "the whole mebibyte lands"
    );

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Audit `docs/audits/third-run-engine.md`, finding 5: `forget` and the pool.
// ---------------------------------------------------------------------------

/// `forget` must close the device's pool, not only drop the registry's
/// handle on it.
///
/// A connection Finder already holds keeps its own `Arc<Bridge>`, which
/// keeps the `Arc<Pool>`. Before this, the next request on that connection
/// popped an idle pooled connection `forget` had never closed, so files on
/// a forgotten device were still read after `forget` returned.
#[test]
fn forget_closes_the_devices_pool_so_an_open_bridge_serves_nothing_more() {
    let contents = b"the phone's own notes".as_slice();
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key_pair = generate_key().expect("a fresh key pair");
    let mac = common::build_side(
        "Vamana",
        DeviceKind::Mac,
        mac_key.clone(),
        &[],
        &phone_key_pair,
        "Pixel 3 XL",
        DeviceKind::Phone,
    );
    let phone = common::build_side(
        "Pixel 3 XL",
        DeviceKind::Phone,
        phone_key_pair,
        &[("Notes.txt", contents)],
        &mac_key,
        "Vamana",
        DeviceKind::Mac,
    );
    phone.engine.set_reachable(true);

    let phone_key = mac.engine.devices()[0].key_hex.clone();
    // Discovery is not running in a test, so this `list` is what gives the
    // Mac the phone's address and marks it reachable.
    mac.engine.offer_candidate(common::loopback_addr(&phone));
    mac.engine
        .list(phone_key.clone(), String::new())
        .expect("listing the phone's root should succeed once dialable");
    let endpoint = mac
        .engine
        .mount_start(phone_key.clone())
        .expect("mount_start should succeed for a paired, reachable device");

    let port = common::port_of(&endpoint.url);
    let host = format!("127.0.0.1:{port}");
    let addr: SocketAddr = host.parse().expect("a loopback address");
    let mut client = common::TestClient::connect(addr);
    let auth = Some((endpoint.user.as_str(), endpoint.password.as_str()));

    let served = client.request("GET", "/Root/Notes.txt", &host, auth, &[], None);
    assert_eq!(
        served.status, 200,
        "the bridge serves the file while paired"
    );
    assert_eq!(served.body, contents, "and serves its real bytes");

    mac.engine
        .forget(phone_key)
        .expect("forgetting a paired device should succeed");

    let refused = client.request("GET", "/Root/Notes.txt", &host, auth, &[], None);
    assert_eq!(
        refused.status, 503,
        "the next request on the same connection must be refused"
    );
    assert!(
        refused.body.is_empty(),
        "a forgotten device's files must not be read again"
    );

    // The bridge's own port went with `forget`, so a fresh request cannot
    // even reach it.
    assert!(
        std::net::TcpStream::connect(addr).is_err(),
        "the forgotten device's bridge port must be closed"
    );

    mac.engine.stop();
    phone.engine.stop();
}

// ---------------------------------------------------------------------------
// Audit `docs/audits/third-run-engine.md`, finding 10: a bridge after `stop`.
// ---------------------------------------------------------------------------

/// A `mount_start` that lands after `stop` began must be refused.
///
/// `Engine::stop` copies the keys of every running bridge and then stops
/// each one. A bridge started after that copy was never joined, so one
/// loopback port and two threads stayed alive after `stop` returned, each
/// holding a handle on the engine's shared state.
#[test]
fn a_stopped_engine_starts_no_new_bridge() {
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone_key = pair(&mac, &phone);

    mac.engine.stop();

    let error = mac
        .engine
        .mount_start(phone_key)
        .expect_err("a stopped engine should start no bridge");
    assert_eq!(code_of_error(&error), "Runtime::NotReachable");

    phone.engine.stop();
}
