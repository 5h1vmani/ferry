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
//! `Inbox`, `Recorder`, `build_as` and `pair` come from
//! `tests/common/engines.rs`, which every two-engine test file shares.

mod common;

use common::engines::{Inbox, Recorder, Side, build, build_as, code_of_error, pair};

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use ferry_runtime::{
    Config, DeviceKind, Engine, KeyPair, Root, browse_allowed_rule, generate_key, welcomes_inbound,
    wifi_presence_rule,
};

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
        shared_root: side.shared.path().to_path_buf(),
        download_root: side.download.path().to_path_buf(),
        data: side.data,
        shared: side.shared,
        download: side.download,
    }
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

#[test]
fn browsing_does_not_need_reachable() {
    // A Mac with its presence switch off, and an empty trusted list, still
    // browses: this is what let it find and mount a phone before item 18,
    // and `reachable` off must not take that away.
    assert!(
        browse_allowed_rule(&[], None, false),
        "an empty trusted list allows browsing with reachable off"
    );
    assert!(
        !wifi_presence_rule(false, &[], None, false),
        "reachable off is still off for Wi-Fi presence, which gates the \
         advertiser and inbound acceptance"
    );

    let home = names(&["Home"]);
    assert!(
        browse_allowed_rule(&home, Some("Home"), false),
        "browsing is allowed on a trusted network"
    );
    assert!(
        !browse_allowed_rule(&home, Some("Cafe"), false),
        "browsing is refused on an untrusted, non-empty list"
    );
    assert!(
        browse_allowed_rule(&home, None, true),
        "pairing in progress allows browsing on any network"
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

#[test]
fn a_wish_set_before_start_survives_start() {
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
        display_name: "Dushyanta".to_owned(),
        listen_port: 0,
        key,
        kind: DeviceKind::Mac,
    };
    let engine = Engine::new(
        config,
        Box::new(Recorder {
            inbox: Arc::clone(&inbox),
        }),
    )
    .expect("the engine should build from a good config");

    // The wish is set before `start`, while there is no port yet for the
    // advertiser to use.
    engine.set_reachable(true);
    engine.start().expect("the engine should start");

    let status = engine.status();
    assert!(status.reachable, "reachable was set before start");
    assert!(
        status.wifi_presence,
        "an empty trusted list is present once reachable"
    );
    // `status().wifi_presence` reads whether an advertiser is actually
    // running (finding 5, `docs/audits/fable-engineering.md`), so the
    // assertion above already is proof that `start` applied the wish.
    // `short_code` reads the same `Shared::advertiser` a different way, and
    // is kept here too: before `start` calls `apply_presence` itself, the
    // wish set here is recorded but never acted on, and the advertiser
    // never starts for the rest of the run.
    assert!(
        engine.short_code().is_some(),
        "start should apply the rule now that it has a port, and start the \
         advertiser the earlier set_reachable(true) could not"
    );

    engine.stop();
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

// ---------------------------------------------------------------------------
// Audit `docs/audits/third-run-engine.md`, finding 2: a control character in
// a network name.
// ---------------------------------------------------------------------------

/// A name holding a newline is one name to the person and two lines in the
/// file. On the next start the second line loads as a network nobody
/// trusted, and `forget_network` with the whole name removes neither half.
#[test]
fn a_name_holding_a_control_character_is_refused_and_never_loads() {
    let side = build("Bharata");

    let refused = side
        .engine
        .trust_network("Home\nCafe".to_owned())
        .expect_err("a name holding a newline cannot be trusted");
    assert_eq!(code_of_error(&refused), "Runtime::NetworkName");
    assert!(
        side.engine.trusted_networks().is_empty(),
        "a refused name is not added"
    );

    side.engine
        .trust_network("Home".to_owned())
        .expect("an ordinary name should be trusted");
    side.engine.stop();

    // A file written by an older build, or by hand, can hold a line this
    // build would refuse. Loading drops that line and keeps the rest.
    let path = side.data.path().join("networks");
    std::fs::write(&path, "Home\nCa\u{7}fe\n").expect("the list file should write");
    let inbox = Arc::new(Inbox::default());
    let engine = open(
        "Bharata",
        DeviceKind::Mac,
        &side.key,
        &inbox,
        side.data.path(),
        side.shared.path(),
        side.download.path(),
    );
    assert_eq!(
        engine.trusted_networks(),
        names(&["Home"]),
        "a stored line holding a control character is skipped, not loaded"
    );
    engine.stop();
}

// ---------------------------------------------------------------------------
// Audit `docs/audits/third-run-engine.md`, finding 8: presence after `stop`.
// ---------------------------------------------------------------------------

/// Nothing an app calls after `stop` may start the advertiser again.
///
/// `stop` never cleared `listen_addr`, so a `set_reachable(true)` that
/// arrived after it found `reachable` true, an empty trusted list, and a
/// port to announce. mDNS then kept announcing a closed port until the
/// process exited.
#[test]
fn a_stopped_engine_never_advertises_again() {
    let side = build("Shantanu");
    side.engine.set_reachable(true);
    assert!(
        side.engine.short_code().is_some(),
        "a reachable engine advertises, so there is something to turn off"
    );

    side.engine.stop();
    side.engine.set_reachable(true);

    assert!(
        side.engine.short_code().is_none(),
        "a stopped engine must not announce a port it has already closed"
    );
    assert!(
        !side.engine.status().wifi_presence,
        "a stopped engine is present on no network"
    );
}
