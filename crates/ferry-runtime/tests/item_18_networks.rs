//! Trusted Wi-Fi networks. `docs/engine-contract.md`, item 18.
//!
//! A device that paired at home stays silent in a café. This file proves the
//! engine half: the rule that decides Wi-Fi presence, the list that survives
//! a restart, the refusals a bad name earns, what `status()` reports, and the
//! network a pairing records on both sides.
//!
//! No test here sends a packet off this machine. The rule and the welcome
//! decision are pure functions, so both are called directly with the inputs
//! each branch needs, including a non-loopback address that is never dialed.
//! The one test that pairs two engines does it over loopback, the way every
//! other test file in this crate does.
//!
//! `Inbox`, `Recorder`, `build_as` and `pair` are copied from
//! `two_engines.rs`, as every test file in this crate copies them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferry_runtime::{
    Config, DeviceKind, Engine, EngineListener, KeyPair, PairingCandidate, PairingMethod,
    PairingState, Root, generate_key, welcomes_inbound, wifi_presence_rule,
};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(10);

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
    key: KeyPair,
    /// Held so the folders live as long as the engine does.
    data: tempfile::TempDir,
    shared: tempfile::TempDir,
    download: tempfile::TempDir,
}

/// Build and start one engine, of the given device kind, on fresh folders.
fn build_as(name: &str, kind: DeviceKind) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key: KeyPair = generate_key().expect("a fresh key pair");
    let inbox = Arc::new(Inbox::default());
    let engine = open(
        name,
        kind,
        &key,
        &inbox,
        data.path(),
        shared.path(),
        download.path(),
    );
    Side {
        engine,
        inbox,
        key,
        data,
        shared,
        download,
    }
}

/// Build and start one engine, of the given device kind, as a Mac.
fn build(name: &str) -> Side {
    build_as(name, DeviceKind::Mac)
}

/// Build and start one engine on folders that already exist.
fn open(
    name: &str,
    kind: DeviceKind,
    key: &KeyPair,
    inbox: &Arc<Inbox>,
    data: &std::path::Path,
    shared: &std::path::Path,
    download: &std::path::Path,
) -> Arc<Engine> {
    let config = Config {
        data_dir: data.to_string_lossy().into_owned(),
        shared_roots: vec![Root {
            name: "Root".to_owned(),
            path: shared.to_string_lossy().into_owned(),
            writable: true,
        }],
        download_dir: download.to_string_lossy().into_owned(),
        display_name: name.to_owned(),
        listen_port: 0,
        key: key.clone(),
        kind,
    };
    let engine = Engine::new(
        config,
        Box::new(Recorder {
            inbox: Arc::clone(inbox),
        }),
    )
    .expect("the engine should build from a good config");
    engine.start().expect("the engine should start");
    engine
}

/// Stop `side`'s engine and build a fresh one on the very same folders, with
/// the same key. This is what an app relaunch looks like to the engine.
fn restart(side: Side, name: &str) -> Side {
    side.engine.stop();
    let inbox = Arc::new(Inbox::default());
    let engine = open(
        name,
        DeviceKind::Mac,
        &side.key,
        &inbox,
        side.data.path(),
        side.shared.path(),
        side.download.path(),
    );
    Side {
        engine,
        inbox,
        key: side.key,
        data: side.data,
        shared: side.shared,
        download: side.download,
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

/// Pair `mac` with `phone` and return the phone's key hex, as `mac` names it.
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
    let chosen: &PairingCandidate = candidates
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

/// A list of trusted names, as the rule takes it.
fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// The rule, one case per branch.
// ---------------------------------------------------------------------------

#[test]
fn the_wifi_presence_rule_covers_every_branch() {
    let home = names(&["Home"]);

    // `reachable` off is off, whatever else holds.
    assert!(
        !wifi_presence_rule(false, &[], None, true),
        "presence needs reachable, even with an empty list and pairing running"
    );

    // An empty list trusts every network, known or not.
    assert!(
        wifi_presence_rule(true, &[], None, false),
        "an empty trusted list is on, which is how it was before item 18"
    );
    assert!(
        wifi_presence_rule(true, &[], Some("Cafe"), false),
        "an empty trusted list is on for any network"
    );

    // A network in the list is on.
    assert!(
        wifi_presence_rule(true, &home, Some("Home"), false),
        "the current network is in the list"
    );

    // A network that is not in the list is off.
    assert!(
        !wifi_presence_rule(true, &home, Some("Cafe"), false),
        "a network that is not trusted is off"
    );

    // An unknown network with a non-empty list is off.
    assert!(
        !wifi_presence_rule(true, &home, None, false),
        "an unknown network with a non-empty list is off"
    );

    // Pairing in progress is on, whatever the network is.
    assert!(
        wifi_presence_rule(true, &home, Some("Cafe"), true),
        "pairing in progress is on, so a person can pair in a café"
    );
    assert!(
        wifi_presence_rule(true, &home, None, true),
        "pairing in progress is on even when the network is unknown"
    );
}

// ---------------------------------------------------------------------------
// The welcome decision, with a loopback and a non-loopback address.
// ---------------------------------------------------------------------------

#[test]
fn the_welcome_decision_always_takes_the_cable() {
    let cable = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4242);
    let cable_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4242);
    let network = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 5)), 4242);

    // Job 2: loopback is the adb tunnel, so the cable works while this
    // device is quiet on the Wi-Fi network it is on.
    assert!(
        welcomes_inbound(true, false, false, cable),
        "a loopback address is welcome while presence is off"
    );
    assert!(
        welcomes_inbound(true, false, false, cable_v6),
        "an IPv6 loopback address is welcome too"
    );

    // The café: reachable, presence off, nobody pairing, an address off
    // this machine.
    assert!(
        !welcomes_inbound(true, false, false, network),
        "a non-loopback address is refused while presence is off"
    );

    // `reachable` off is off, over the cable as well. This is what
    // `set_reachable(false)` has always meant.
    assert!(
        !welcomes_inbound(false, false, false, cable),
        "the reachable switch refuses the cable too"
    );
    assert!(
        !welcomes_inbound(false, false, false, network),
        "the reachable switch refuses the network too"
    );

    // Presence on welcomes the network.
    assert!(
        welcomes_inbound(true, true, false, network),
        "a non-loopback address is welcome while presence is on"
    );

    // A pairing open to an inbound handshake welcomes the network too. This
    // is the half of the check that was there before item 18.
    assert!(
        welcomes_inbound(false, false, true, network),
        "an open pairing welcomes a non-loopback address"
    );
}

// ---------------------------------------------------------------------------
// Trust, forget, and the list on disk.
// ---------------------------------------------------------------------------

#[test]
fn a_trusted_network_survives_a_restart() {
    let side = build("Bharata");
    assert!(
        side.engine.trusted_networks().is_empty(),
        "a fresh data folder has no networks file, so the list is empty"
    );

    side.engine
        .trust_network("Home".to_owned())
        .expect("a plain name should be trusted");
    side.engine
        .trust_network("Work".to_owned())
        .expect("a second plain name should be trusted");
    assert_eq!(side.engine.trusted_networks(), names(&["Home", "Work"]));

    // Trusting the same name twice is not an error and does not grow it.
    side.engine
        .trust_network("Home".to_owned())
        .expect("a name already trusted is not an error");
    assert_eq!(side.engine.trusted_networks(), names(&["Home", "Work"]));

    side.engine
        .forget_network("Home".to_owned())
        .expect("a trusted name should be forgotten");
    // Forgetting a name that is not there leaves the same list.
    side.engine
        .forget_network("Nowhere".to_owned())
        .expect("a name that is not trusted is not an error");
    assert_eq!(side.engine.trusted_networks(), names(&["Work"]));

    let side = restart(side, "Bharata");
    assert_eq!(
        side.engine.trusted_networks(),
        names(&["Work"]),
        "the list is read from data_dir/networks at start"
    );
    side.engine.stop();
}

#[test]
fn a_refused_network_name_says_why() {
    let side = build("Nahusha");

    let empty = side
        .engine
        .trust_network(String::new())
        .expect_err("an empty name cannot be trusted");
    assert_eq!(code_of_error(&empty), "Runtime::NetworkName");

    // Thirty-two bytes is the limit, so thirty-two is taken and
    // thirty-three is refused.
    side.engine
        .trust_network("a".repeat(32))
        .expect("a name of exactly 32 bytes should be trusted");
    let long = side
        .engine
        .trust_network("a".repeat(33))
        .expect_err("a name over 32 bytes cannot be trusted");
    assert_eq!(code_of_error(&long), "Runtime::NetworkName");

    // Fill the list to its 32 names, then ask for a 33rd.
    for index in 1..32 {
        side.engine
            .trust_network(format!("net-{index}"))
            .unwrap_or_else(|error| panic!("name {index} should be trusted: {error:?}"));
    }
    assert_eq!(side.engine.trusted_networks().len(), 32);
    let full = side
        .engine
        .trust_network("one-too-many".to_owned())
        .expect_err("a 33rd name cannot be trusted");
    assert_eq!(code_of_error(&full), "Runtime::NetworkName");
    assert_eq!(
        side.engine.trusted_networks().len(),
        32,
        "a refused name is not added"
    );

    side.engine.stop();
}

// ---------------------------------------------------------------------------
// What `status()` reports.
// ---------------------------------------------------------------------------

#[test]
fn setting_the_network_turns_wifi_presence_off_and_on() {
    let side = build("Yayati");

    let before = side.engine.status();
    assert!(!before.reachable, "an engine starts unreachable");
    assert!(!before.wifi_presence, "unreachable means no presence");
    assert_eq!(before.network, None, "the app has set no name yet");

    side.engine.set_reachable(true);
    assert!(
        side.engine.status().wifi_presence,
        "reachable with an empty trusted list is present, as before item 18"
    );

    side.engine
        .trust_network("Home".to_owned())
        .expect("a plain name should be trusted");
    assert!(
        !side.engine.status().wifi_presence,
        "an unknown network with a non-empty list is off"
    );

    side.engine.set_network(Some("Cafe".to_owned()));
    let cafe = side.engine.status();
    assert_eq!(cafe.network, Some("Cafe".to_owned()));
    assert!(!cafe.wifi_presence, "a network that is not trusted is off");

    side.engine.set_network(Some("Home".to_owned()));
    let home = side.engine.status();
    assert_eq!(home.network, Some("Home".to_owned()));
    assert!(home.wifi_presence, "a trusted network is on");

    side.engine.set_network(None);
    let unknown = side.engine.status();
    assert_eq!(unknown.network, None, "the name went back to unknown");
    assert!(!unknown.wifi_presence, "an unknown name is off again");

    side.engine.set_network(Some("Home".to_owned()));
    side.engine.set_reachable(false);
    assert!(
        !side.engine.status().wifi_presence,
        "the reachable switch still turns everything off"
    );

    side.engine.stop();
}

// ---------------------------------------------------------------------------
// What a pairing records.
// ---------------------------------------------------------------------------

#[test]
fn pairing_trusts_the_network_on_both_sides() {
    let mac = build_as("Sagara", DeviceKind::Mac);
    let phone = build_as("Amsumant", DeviceKind::Phone);

    mac.engine.set_network(Some("Home".to_owned()));
    phone.engine.set_network(Some("Home".to_owned()));
    assert!(mac.engine.trusted_networks().is_empty());
    assert!(phone.engine.trusted_networks().is_empty());

    drop(pair(&mac, &phone));

    assert_eq!(
        mac.engine.trusted_networks(),
        names(&["Home"]),
        "the Mac trusts the network it paired on"
    );
    assert_eq!(
        phone.engine.trusted_networks(),
        names(&["Home"]),
        "the phone trusts the network it paired on"
    );

    // The phone is reachable and on a network it now trusts, so it stays
    // present once pairing has ended.
    assert!(
        phone.engine.status().wifi_presence,
        "the network the pairing recorded keeps the phone present"
    );

    mac.engine.stop();
    phone.engine.stop();
}
