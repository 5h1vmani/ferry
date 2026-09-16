//! The `WebDAV` bridge's spool folder byte count, kept running instead of
//! walked on every `PUT` and `COPY`. `docs/audits/fable-engineering.md`,
//! finding 4.
//!
//! Before this fix, every `PUT` and `COPY` walked the whole spool folder
//! recursively to sum its bytes, on every request. This file proves the
//! running count `Engine::spool_bytes` now reports instead: it stays exact
//! across two ordinary `PUT`s, and a spool file removed out from under a
//! failed one lowers it, rather than a bug leaving it to only ever grow.
//!
//! The engines, the folders, and the HTTP client this file drives are
//! `tests/common/mod.rs`'s, the same ones the `dav_*.rs` files use for the
//! bridge's other tests.

mod common;

use ferry_runtime::{DeviceKind, generate_key};

use common::paths::poll_until;
use common::{TestClient, base64_encode, build_side, loopback_addr, pattern, port_of};

/// The spool folder's total size right now, walked directly by the test
/// rather than through the engine. Kept independent of
/// `Engine::spool_bytes` and `crates/ferry-runtime/src/dav/put.rs`'s own
/// `dir_bytes`, so a bug that always answered a fixed number would show up
/// as a mismatch here instead of passing by construction.
fn real_spool_bytes(data_dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![data_dir.join("dav_spool")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

/// Waits for `Engine::spool_bytes` to agree with a fresh walk of the spool
/// folder, naming `what` in the panic if it never does.
///
/// `SpoolFile`'s `Drop` runs after `put_file` writes its response to the
/// socket, not before, so a client that has just read a `PUT`'s response
/// has no guarantee the server thread has already finished unwinding and
/// dropped that request's spool file. Polling for agreement, instead of
/// asserting it the instant the response arrives, tests the real
/// guarantee this fix makes, that the count converges to the truth
/// shortly after landing, rather than a synchronous ordering nothing ever
/// promised.
fn wait_for_count_to_match_disk(
    engine: &ferry_runtime::Engine,
    data_dir: &std::path::Path,
    what: &str,
) {
    poll_until(what, || engine.spool_bytes() == real_spool_bytes(data_dir));
    assert_eq!(engine.spool_bytes(), real_spool_bytes(data_dir), "{what}");
}

#[test]
fn two_puts_in_a_row_leave_the_running_count_exact_and_a_removed_spool_file_lowers_it() {
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
        &[("Existing.txt", b"the original bytes")],
        &mac_key,
        "Vamana",
        DeviceKind::Mac,
    );
    phone.engine.set_reachable(true);

    let phone_key_hex = mac
        .engine
        .devices()
        .first()
        .expect("the phone should already be paired, from the seeded peer store")
        .key_hex
        .clone();
    mac.engine.offer_candidate(loopback_addr(&phone));
    mac.engine
        .list(phone_key_hex.clone(), String::new())
        .expect("listing the phone's root should succeed once dialable");

    // The bridge for the phone runs on the Mac side, so the Mac's own
    // data folder is where `dav_spool/` lives, and where `Engine::spool_bytes`
    // reads from.
    let endpoint = mac
        .engine
        .mount_start(phone_key_hex)
        .expect("mount_start should succeed for a paired, reachable device");
    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");
    let host = format!("127.0.0.1:{}", port_of(&endpoint.url));
    let auth = (endpoint.user.as_str(), endpoint.password.as_str());

    assert_eq!(
        mac.engine.spool_bytes(),
        0,
        "no PUT has happened yet, so the spool folder is empty"
    );

    // --- Two ordinary PUTs in a row, each fully landed before the next
    // starts, exactly as `tests/dav_write.rs`'s I2 test does it. Every spool
    // file created along the way is removed once its landing finishes
    // (`put.rs`'s `SpoolFile` drops at the end of `put_file`), so the
    // running count returns to what the folder really holds, not to
    // whatever the walk happened to see last.
    let mut client = TestClient::connect(addr);
    for (name, len) in [("First.bin", 5_000usize), ("Second.bin", 9_000usize)] {
        let bytes = pattern(len);
        let response = client.request(
            "PUT",
            &format!("/Root/{name}"),
            &host,
            Some(auth),
            &[],
            Some(&bytes),
        );
        assert_eq!(response.status, 201, "the PUT of {name} should land");
        wait_for_count_to_match_disk(
            &mac.engine,
            mac.data.path(),
            &format!("the running count to match the spool folder's real size after {name} lands"),
        );
    }
    // Both landings are long since finished (the spool folder holds
    // nothing of its own; the bytes live on the phone now), so this is a
    // real check that the count did not drift upward across two requests,
    // not a check that only ever compares zero to zero by construction.
    assert_eq!(mac.engine.spool_bytes(), 0);
    assert_eq!(real_spool_bytes(mac.data.path()), 0);

    // --- A PUT whose body never arrives: the spool file `new_spool_path`
    // reserves for it lands, then is removed. This proves the count goes
    // up when a spool file is created and back down when it is removed,
    // not just that it never moves at all.
    let reserved_len = 4_096u64;
    let credentials = base64_encode(format!("{}:{}", auth.0, auth.1).as_bytes());
    let mut aborted = TestClient::connect(addr);
    aborted.write_raw_head(&format!(
        "PUT /Root/Aborted.bin HTTP/1.1\r\nHost: {host}\r\nAuthorization: Basic {credentials}\r\nContent-Length: {reserved_len}\r\n\r\n"
    ));
    poll_until("the reserved spool file to raise the running count", || {
        mac.engine.spool_bytes() >= reserved_len
    });

    // The body never comes: closing the connection here is `put.rs`'s own
    // "a short body, a dropped connection ... must never leave a spool
    // file behind." `spool_body` reads zero of the declared
    // `reserved_len` bytes, errors, and ends the connection, dropping the
    // `SpoolFile` and lowering the count by the same amount it raised it.
    drop(aborted);

    poll_until(
        "the dropped spool file to lower the running count back down",
        || mac.engine.spool_bytes() == 0,
    );
    assert_eq!(
        real_spool_bytes(mac.data.path()),
        0,
        "the aborted PUT's spool file must not survive on disk either"
    );

    mac.engine.stop();
    phone.engine.stop();
}
