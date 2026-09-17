//! What the `WebDAV` bridge refuses when a bound is reached.
//!
//! `docs/engine-contract.md`, item 6, and `docs/audits/fable-security.md`:
//! the spool folder has a cap and a dropped body is cleaned up, a leftover
//! that already fills the cap refuses the next `PUT`, and the thirty-third
//! idle connection is closed at once.
//!
//! The engines, the folders, and the HTTP client this file drives live in
//! `tests/common/mod.rs`.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use ferry_runtime::{DeviceKind, generate_key};

use common::{PATIENCE, TestClient, base64_encode, build_side, loopback_addr, pattern, port_of};

#[test]
// I2-3: the spool file is never left behind. The folder's total size cap
// is its own test, `put_refuses_once_a_leftover_already_fills_the_spool_cap`
// below: `docs/audits/fable-engineering.md`, finding 4, moved the cap check
// from a walk on every `PUT` to a running count seeded by one walk at the
// first `mount_start`, which this test's own `mount_start` call, already
// past by the time a `PUT` here could plant a sparse file, can no longer
// see.
fn put_bounds_the_spool_folder_and_cleans_up_a_dropped_body() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Bounds Mac",
        DeviceKind::Mac,
        mac_key.clone(),
        &[],
        &phone_key,
        "Bounds Phone",
        DeviceKind::Phone,
    );
    let phone = build_side(
        "Bounds Phone",
        DeviceKind::Phone,
        phone_key,
        &[],
        &mac_key,
        "Bounds Mac",
        DeviceKind::Mac,
    );
    phone.engine.set_reachable(true);
    let key_hex = mac
        .engine
        .devices()
        .first()
        .expect("the phone should already be paired")
        .key_hex
        .clone();
    mac.engine.offer_candidate(loopback_addr(&phone));
    mac.engine
        .list(key_hex.clone(), String::new())
        .expect("listing should succeed once dialable");
    let endpoint = mac
        .engine
        .mount_start(key_hex.clone())
        .expect("mount_start should succeed");
    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");
    let host = format!("127.0.0.1:{}", port_of(&endpoint.url));
    let credentials = base64_encode(format!("{}:{}", endpoint.user, endpoint.password).as_bytes());

    // --- A body over `MAX_PUT_BODY_LEN` (32 GiB) is 413, before a single
    // byte reaches the spool file. `MAX_PUT_BODY_LEN` is private to
    // `ferry_runtime`, so its value, one past the 32 GiB cap, is spelled
    // out here instead.
    let mut client = TestClient::connect(addr);
    client.write_raw_head(&format!(
        "PUT /Root/Huge.bin HTTP/1.1\r\nHost: {host}\r\n\
         Authorization: Basic {credentials}\r\n\
         Content-Length: 34359738369\r\n\r\n"
    ));
    let response = client.read_response(false);
    assert_eq!(response.status, 413);

    // --- A dropped connection mid body leaves no spool file behind.
    let mut client = TestClient::connect(addr);
    client.write_raw_head(&format!(
        "PUT /Root/Partial.bin HTTP/1.1\r\nHost: {host}\r\n\
         Authorization: Basic {credentials}\r\n\
         Content-Length: 1000\r\n\r\n"
    ));
    client
        .write_half
        .write_all(&pattern(10))
        .expect("a short partial body should write");
    drop(client);

    let spool_dir = mac.data.path().join("dav_spool").join(&key_hex);
    let deadline = Instant::now() + PATIENCE;
    loop {
        let remaining = std::fs::read_dir(&spool_dir)
            .map(Iterator::count)
            .unwrap_or(0);
        if remaining == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a dropped connection must not leave a spool file behind"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    phone.engine.stop();
    mac.engine.stop();
}

#[test]
// I2-3: the spool folder's total size cap answers 507.
//
// `docs/audits/fable-engineering.md`, finding 4: the running count
// `new_spool_path` checks against is seeded by one walk of the whole
// `dav_spool` folder, the first time any bridge on this engine starts
// (`dav::MountRegistry::start`), not by a fresh walk on every `PUT`. So
// this plants its sparse, 64 GiB leftover under a device key that is not
// the one about to mount, before `mount_start` ever runs: a real leftover
// from a crash sits exactly like this, under whichever device's folder
// crashed, until that device's own bridge starts and sweeps it, and the
// cap must still see it in the meantime. Planting it under the mounting
// device's own key would not prove anything, since `MountRegistry::start`
// sweeps that key's folder, real leftover or test fixture alike, before
// the one walk ever runs.
fn put_refuses_once_a_leftover_already_fills_the_spool_cap() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Cap Mac",
        DeviceKind::Mac,
        mac_key.clone(),
        &[],
        &phone_key,
        "Cap Phone",
        DeviceKind::Phone,
    );
    let phone = build_side(
        "Cap Phone",
        DeviceKind::Phone,
        phone_key,
        &[],
        &mac_key,
        "Cap Mac",
        DeviceKind::Mac,
    );
    phone.engine.set_reachable(true);
    let key_hex = mac
        .engine
        .devices()
        .first()
        .expect("the phone should already be paired")
        .key_hex
        .clone();

    // A sparse file reports the size the running count is seeded with
    // without this test writing anywhere near 64 GiB of real bytes. It
    // sits under a device key distinct from `key_hex`, so the sweep
    // `mount_start` runs for `key_hex` never touches it, and the one walk
    // that follows the sweep counts it.
    let leftover_dir = mac.data.path().join("dav_spool").join("leftover-device");
    std::fs::create_dir_all(&leftover_dir).expect("the leftover folder should create");
    let sparse = leftover_dir.join("already-huge");
    let file = std::fs::File::create(&sparse).expect("a sparse file should create");
    file.set_len(64 * 1024 * 1024 * 1024)
        .expect("a sparse file should grow without writing real bytes");
    drop(file);

    mac.engine.offer_candidate(loopback_addr(&phone));
    mac.engine
        .list(key_hex.clone(), String::new())
        .expect("listing should succeed once dialable");
    let endpoint = mac
        .engine
        .mount_start(key_hex)
        .expect("mount_start should succeed, and walk the leftover into the running count");
    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");
    let host = format!("127.0.0.1:{}", port_of(&endpoint.url));

    let mut client = TestClient::connect(addr);
    let response = client.request(
        "PUT",
        "/Root/OneMore.bin",
        &host,
        Some((endpoint.user.as_str(), endpoint.password.as_str())),
        &[],
        Some(&pattern(10)),
    );
    assert_eq!(response.status, 507);

    phone.engine.stop();
    mac.engine.stop();
}

#[test]
fn the_thirty_third_idle_connection_is_closed_at_once() {
    // B3: this bridge serves this many connections at once; one more is
    // refused at accept, before its socket is ever read from.
    const MAX_LIVE_CONNECTIONS: usize = 32;

    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Vamana",
        DeviceKind::Mac,
        mac_key.clone(),
        &[],
        &phone_key,
        "Pixel 3 XL",
        DeviceKind::Phone,
    );
    let phone = build_side(
        "Pixel 3 XL",
        DeviceKind::Phone,
        phone_key.clone(),
        &[("Notes.txt", b"hi")],
        &mac_key,
        "Vamana",
        DeviceKind::Mac,
    );
    phone.engine.set_reachable(true);
    let phone_key_hex = mac
        .engine
        .devices()
        .first()
        .expect("paired")
        .key_hex
        .clone();
    mac.engine.offer_candidate(loopback_addr(&phone));
    mac.engine
        .list(phone_key_hex.clone(), String::new())
        .expect("listing should succeed");

    let endpoint = mac.engine.mount_start(phone_key_hex).expect("mount_start");
    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");

    // 32 plain connections, sending nothing, hold every slot:
    // `accept_loop` is one thread accepting strictly in order, so all 32
    // are fully reserved before it ever looks at a 33rd. No sleep is
    // needed here: the kernel's own accept backlog is a FIFO queue for
    // this one listener, so the 33rd connection below cannot be dequeued
    // ahead of the first 32, however slow or busy `accept_loop`'s thread
    // is. The read below, which blocks for up to `PATIENCE`, is what
    // waits out any real delay in `accept_loop` catching up.
    let mut idle = Vec::with_capacity(MAX_LIVE_CONNECTIONS);
    for _ in 0..MAX_LIVE_CONNECTIONS {
        idle.push(TcpStream::connect(addr).expect("the bridge should accept up to its cap"));
    }

    let mut refused =
        TcpStream::connect(addr).expect("the 33rd connection should still complete its handshake");
    refused
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout should set");
    let mut buf = [0u8; 1];
    let read = refused
        .read(&mut buf)
        .expect("a closed socket should read as a clean EOF, not an error");
    assert_eq!(read, 0, "the 33rd live connection should be closed at once");

    drop(idle);
    mac.engine.stop();
    phone.engine.stop();
}
