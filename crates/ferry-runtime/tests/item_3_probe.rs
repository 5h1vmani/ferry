//! Item 3: the reachability probe a discovered address starts.
//!
//! `docs/engine-contract.md`, item 3. Before this, every caller of the
//! engine's `mark_reachable` was a dial made for work, or a connection
//! accepted and served. A device that only found its peer on the network
//! stayed "not reachable" until that peer happened to connect, so the first
//! open of the phone's Files app after a restart said "not reachable".
//!
//! Two engines in one process, from `tests/common/engines.rs`. No test
//! sleeps and hopes, and no test sends a packet off this machine: every
//! engine binds `127.0.0.1` on a port the operating system picks, and the
//! one wait is a condition variable with a generous deadline.
//!
//! The discovery event is `Engine::offer_candidate`, the same seam every
//! other integration test here uses to stand in for a browse. It is the one
//! entry point mDNS's own `on_discovered` shares, `remember_address`, so it
//! starts a probe exactly the way a real advert does.

mod common;

use common::engines::{Inbox, Recorder, Side, build_as, loopback_addr, pair};

use std::net::SocketAddr;
use std::sync::Arc;

use ferry_runtime::{Config, DeviceKind, Engine, Root};

/// A second engine on the folders a stopped one left behind, as if the app
/// had been restarted.
///
/// The restart is what puts the phone in the state the owner reported: the
/// Mac is still a stored paired device, but nothing is marked reachable,
/// because reachability is learned per run and never written down.
fn restart(side: &Side, name: &str, kind: DeviceKind) -> (Arc<Engine>, Arc<Inbox>) {
    let inbox = Arc::new(Inbox::default());
    let engine = Engine::new(
        Config {
            data_dir: side.data.path().to_string_lossy().into_owned(),
            shared_roots: vec![Root {
                name: "Root".to_owned(),
                path: side.shared.path().to_string_lossy().into_owned(),
                writable: true,
            }],
            download_dir: side.download.path().to_string_lossy().into_owned(),
            display_name: name.to_owned(),
            listen_port: 0,
            key: side.key.clone(),
            kind,
        },
        Box::new(Recorder {
            inbox: Arc::clone(&inbox),
        }),
    )
    .expect("the engine should build on the folder it left");
    engine.start().expect("the engine should start again");
    (engine, inbox)
}

/// `docs/engine-contract.md`, item 3: discovery alone makes this device
/// probe, so a paired device is reported reachable without anyone asking
/// for a file.
///
/// Before the probe, the restarted phone knew the Mac's address and still
/// reported `reachable_via` as `None`, because nothing but a dial for work
/// or an accepted connection ever called `mark_reachable`. This test waits
/// for the phone to report the Mac reachable while neither side lists,
/// pulls, or pushes anything.
#[test]
fn discovery_alone_makes_the_phone_report_the_mac_reachable() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    pair(&mac, &phone);
    // The Mac has to accept, since the phone is the side that dials here.
    mac.engine.set_reachable(true);
    let mac_addr = loopback_addr(&mac);

    // The restart clears what the pairing handshake taught the phone.
    phone.engine.stop();
    let (phone_again, phone_inbox) = restart(&phone, "Pixel 3 XL", DeviceKind::Phone);
    let listed = phone_again.devices();
    assert_eq!(listed.len(), 1, "the Mac is still paired after the restart");
    assert!(
        listed[0].reachable_via.is_none(),
        "a restarted engine starts with nothing marked reachable"
    );

    // One discovery event, and nothing else.
    phone_again.offer_candidate(mac_addr);

    let watching = Arc::clone(&phone_again);
    phone_inbox.wait_until("the phone to report the Mac reachable", move || {
        watching
            .devices()
            .first()
            .is_some_and(|device| device.reachable_via.is_some())
    });

    assert!(
        phone_again.transfers().is_empty(),
        "a probe queues no transfer"
    );
    assert!(
        mac.engine.transfers().is_empty(),
        "the Mac makes no transfer of its own either"
    );

    phone_again.stop();
    mac.engine.stop();
}

/// `docs/engine-contract.md`, item 3: two discovery events inside
/// `PROBE_MIN_INTERVAL_SECS` start one probe, not two.
///
/// The second event carries a different address, so the newest-address
/// check cannot be what suppresses it; only the interval can. The count is
/// read through `Engine::probes`, which is raised before a probe's thread is
/// spawned, so both reads below are exact the moment `offer_candidate`
/// returns. The Mac's own accepted connection count cannot answer this: a
/// second probe would borrow the connection the first one left idle in the
/// pool and accept nothing new.
#[test]
fn two_discovery_events_inside_the_interval_start_one_probe() {
    let mac = build_as("Vamana", DeviceKind::Mac);
    let phone = build_as("Pixel 3 XL", DeviceKind::Phone);
    pair(&mac, &phone);
    mac.engine.set_reachable(true);
    let mac_addr = loopback_addr(&mac);

    phone.engine.stop();
    let (phone_again, phone_inbox) = restart(&phone, "Pixel 3 XL", DeviceKind::Phone);
    assert_eq!(phone_again.probes(), 0, "nothing has been probed yet");

    let accepted_before = mac.engine.accepted_connections();
    phone_again.offer_candidate(mac_addr);
    assert_eq!(
        phone_again.probes(),
        1,
        "the first discovery event starts one probe"
    );

    let watching = Arc::clone(&phone_again);
    phone_inbox.wait_until("the phone to report the Mac reachable", move || {
        watching
            .devices()
            .first()
            .is_some_and(|device| device.reachable_via.is_some())
    });
    assert_eq!(
        mac.engine.accepted_connections(),
        accepted_before + 1,
        "the probe is one connection, and the count has to move for this test to mean anything"
    );

    // A second event, a different address, well inside the interval.
    let other: SocketAddr = "127.0.0.1:9".parse().expect("a literal address parses");
    phone_again.offer_candidate(other);
    assert_eq!(
        phone_again.probes(),
        1,
        "a second discovery event inside PROBE_MIN_INTERVAL_SECS starts no second probe"
    );

    phone_again.stop();
    mac.engine.stop();
}
