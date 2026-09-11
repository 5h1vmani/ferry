//! Pairing paths the happy path does not walk.
//!
//! Audit 2, findings 8, 9 and 14: a capped candidate list, a candidate
//! picked twice, a confirm after the watchdog gave up, and a stranger who
//! learns nothing from the first bytes. Also who an engine accepts a
//! connection from, and what it does with a version 1 peer file.
//!
//! Some tests build a peer by hand. That harness is
//! `tests/common/paths.rs`.

mod common;

use common::paths::{
    Inbox, Recorder, build, code_of_error, is_code, is_confirmed, is_failed, is_found,
    loopback_addr, pair_with_peer, public_key, sample_bytes, start_peer, static_key,
};

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ferry_core::tcp::{self};
use ferry_core::version::{MAGIC, VERSION_MAX};
use ferry_runtime::{
    Config, DeviceKind as RuntimeDeviceKind, Engine, PairingMethod, PairingState, Root,
    generate_key,
};

/// Finding 14: a stranger learns nothing from the first bytes.
///
/// Connect, speak the version exchange, and return what came back.
fn first_bytes_from(addr: SocketAddr) -> Vec<u8> {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("the port should answer");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout should be accepted");
    let mut hello = Vec::with_capacity(8);
    hello.extend_from_slice(&MAGIC);
    hello.extend_from_slice(&VERSION_MAX.to_be_bytes());
    hello.push(0); // Mode::Connect's wire byte.
    // A peer that is not welcome may close before this is written.
    drop(stream.write_all(&hello));
    drop(stream.flush());
    let mut answer = [0u8; 8];
    let mut got = 0usize;
    while got < answer.len() {
        match stream.read(&mut answer[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    answer[..got].to_vec()
}

/// Batch B and C audit, C6: a version 1 peer file assumes this device's
/// opposite kind.
#[test]
fn a_phone_loading_a_version_1_peer_file_assumes_each_peer_is_a_mac() {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared_root = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let key = generate_key().expect("a fresh key pair");
    let peer_key = generate_key().expect("a fresh key pair for the stored peer");

    // A version 1 peer file, written by hand in the format `PeerStore`
    // wrote before item 11 added a kind byte per peer: version, count, then
    // each peer's key, name, and paired time, with no kind byte at all.
    let mut e = ferry_core::wire::Encoder::new();
    e.u8(1);
    e.u32(1);
    e.fixed(public_key(&peer_key).as_bytes());
    e.text("An old Mac");
    e.u64(u64::from_ne_bytes(1_700_000_000i64.to_ne_bytes()));
    std::fs::write(data.path().join("peers.bin"), e.finish())
        .expect("the hand-built version 1 peer file should write");

    let inbox = Arc::new(Inbox::default());
    let engine = Engine::new(
        Config {
            data_dir: data.path().to_string_lossy().into_owned(),
            shared_roots: vec![Root {
                name: "Root".to_owned(),
                path: shared_root.path().to_string_lossy().into_owned(),
                writable: true,
            }],
            download_dir: download.path().to_string_lossy().into_owned(),
            display_name: "Pixel 3 XL".to_owned(),
            listen_port: 0,
            key,
            kind: RuntimeDeviceKind::Phone,
        },
        Box::new(Recorder {
            inbox: Arc::clone(&inbox),
        }),
    )
    .expect("a version 1 peer file should still load");

    let devices = engine.devices();
    assert_eq!(devices.len(), 1, "the stored peer should load");
    assert_eq!(
        devices[0].kind,
        RuntimeDeviceKind::Mac,
        "a version 1 file predates the kind byte; a phone assumes every peer in it is a Mac"
    );
}

/// Finding 8: the candidate list is capped and its reports are rationed.
#[test]
fn the_candidate_list_is_capped_and_its_reports_are_rationed() {
    let mac = build("Vamana");
    mac.engine.set_pairing_timeout(Duration::from_secs(30));
    mac.engine.start_pairing_with(PairingMethod::Code);

    let started = Instant::now();
    for port in 20_000u16..20_200 {
        mac.engine
            .offer_candidate(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    let injected = started.elapsed();

    mac.inbox.wait_pairing("a candidate", is_found);
    // Long enough for a rationed report to arrive after the last candidate.
    std::thread::sleep(Duration::from_millis(1500));

    let founds = mac.inbox.count(is_found);
    assert!(
        founds <= 5,
        "200 candidates in {injected:?} produced {founds} reports"
    );
    let held = mac
        .inbox
        .states()
        .into_iter()
        .filter_map(|state| match state {
            PairingState::Found { candidates, .. } => Some(candidates.len()),
            _ => None,
        })
        .max()
        .expect("at least one report of candidates");
    assert!(held <= 32, "the list must be capped, it held {held}");
    mac.engine.stop();
}

/// Finding 9: one code at a time, and no confirm after the watchdog.
#[test]
fn picking_a_candidate_twice_is_refused_and_shows_one_code() {
    let mac = build("Vamana");
    let peer = start_peer(&mac.key, sample_bytes(16));
    mac.engine.set_pairing_timeout(Duration::from_secs(30));
    mac.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.offer_candidate(peer.addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    peer.expect_pair.store(true, Ordering::SeqCst);

    let id = format!("wifi:{}", peer.addr);
    mac.engine
        .pick_candidate(id.clone())
        .expect("the first pick starts a handshake");
    let second = mac
        .engine
        .pick_candidate(id)
        .expect_err("the second pick must be refused");
    assert_eq!(code_of_error(&second), "Runtime::PairingBusy");

    mac.inbox.wait_pairing("a code", is_code);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        mac.inbox.count(is_code),
        1,
        "only one code may ever be shown"
    );
    mac.engine.stop();
    peer.close();
}

#[test]
fn a_confirm_after_the_watchdog_stores_nothing() {
    let mac = build("Vamana");
    let peer = start_peer(&mac.key, sample_bytes(16));
    // The peer holds its name back, so the confirm is still running when
    // pairing times out.
    *peer
        .hello_delay
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Duration::from_millis(1500);

    mac.engine.set_pairing_timeout(Duration::from_millis(700));
    mac.engine.start_pairing_with(PairingMethod::Code);
    mac.engine.offer_candidate(peer.addr);
    mac.inbox.wait_pairing("a candidate", is_found);
    peer.expect_pair.store(true, Ordering::SeqCst);
    mac.engine
        .pick_candidate(format!("wifi:{}", peer.addr))
        .expect("the candidate should be pickable");
    mac.inbox.wait_pairing("a code", is_code);
    mac.engine.confirm_pairing(true);

    mac.inbox.wait_pairing("the watchdog to give up", is_failed);
    // Long enough for the late name to arrive and be ignored.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        mac.inbox.count(is_confirmed),
        0,
        "a pairing that failed must never confirm"
    );
    assert!(
        mac.engine.devices().is_empty(),
        "a pairing that failed must store no device"
    );
    mac.engine.stop();
    peer.close();
}

#[test]
fn a_stranger_cannot_tell_whether_the_device_has_a_peer() {
    let mut expected = Vec::with_capacity(8);
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&VERSION_MAX.to_be_bytes());
    expected.push(0); // The responder's own mode byte is always this filler.

    let alone = build("Pixel 3 XL");
    alone.engine.set_reachable(true);
    let answer_alone = first_bytes_from(loopback_addr(&alone));

    let paired = build("Pixel 3 XL");
    paired.engine.set_reachable(true);
    let peer = start_peer(&paired.key, sample_bytes(16));
    pair_with_peer(&paired, &peer);
    peer.close();
    let answer_paired = first_bytes_from(loopback_addr(&paired));

    assert_eq!(
        answer_alone, expected,
        "a device with no peer answers the version exchange"
    );
    assert_eq!(
        answer_paired, expected,
        "a device with one peer answers the same bytes"
    );
    alone.engine.stop();
    paired.engine.stop();
}

/// docs/engine-contract.md item 16b: the responder tries every stored key in
/// turn, so a second paired peer is not left unable to connect.
#[test]
fn an_engine_with_two_stored_peers_accepts_a_connection_from_the_second_with_no_address_hint() {
    let side = build("Vamana");
    side.engine.set_reachable(true);

    let first_peer = start_peer(&side.key, sample_bytes(1));
    pair_with_peer(&side, &first_peer);

    // `wait_pairing` finds the first matching state ever recorded, so
    // without this it would see the first pairing's own Found, Code, and
    // Confirmed states and never actually wait for the second one's.
    side.inbox.clear_pairings();
    let second_peer = start_peer(&side.key, sample_bytes(1));
    pair_with_peer(&side, &second_peer);

    // A fresh dial, from a socket the engine has never seen before: nothing
    // recorded favours the second peer's key over the first's. Before item
    // 16b, `choose_peer` guessed one candidate and a wrong guess failed the
    // handshake outright; the responder now tries every stored key against
    // the one message this dial sends.
    tcp::connect(
        loopback_addr(&side),
        &static_key(&second_peer.key),
        &public_key(&side.key),
    )
    .expect("the responder should try every stored key, not only the first");

    side.engine.stop();
    first_peer.close();
    second_peer.close();
}

#[test]
fn a_device_that_is_not_reachable_refuses_a_new_connection() {
    let phone = build("Pixel 3 XL");
    phone.engine.set_reachable(true);
    let peer = start_peer(&phone.key, sample_bytes(16));
    pair_with_peer(&phone, &peer);
    peer.close();

    phone.engine.set_reachable(false);
    let refused = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    );
    assert!(
        refused.is_err(),
        "a device that is not reachable accepts nothing"
    );

    phone.engine.set_reachable(true);
    let accepted = tcp::connect(
        loopback_addr(&phone),
        &static_key(&peer.key),
        &public_key(&phone.key),
    );
    assert!(accepted.is_ok(), "it accepts again once it is reachable");
    phone.engine.stop();
}
