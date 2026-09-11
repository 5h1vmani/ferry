//! The `WebDAV` bridge saving to a device, `docs/engine-contract.md`, item
//! 6, I2.
//!
//! One long narrative test covers `PUT` of a real file, `MKCOL`, `DELETE`,
//! `MOVE`, `COPY` and `PROPPATCH`, and the `If` header's lock check on all
//! of them, in the order a save actually runs in.
//!
//! The engines, the folders, and the HTTP client this file drives live in
//! `tests/common/mod.rs`.

mod common;

use std::net::SocketAddr;

use ferry_runtime::{DeviceKind, generate_key};

use common::{
    RootSpec, TestClient, base64_encode, build_side, build_side_with_roots, count_entries,
    loopback_addr, pattern, port_of, sum_bytes,
};

#[test]
// `docs/engine-contract.md`, item 6, I2: every write verb. One narrative
// test again, for the same reason `the_bridge_serves_a_devices_files_...`
// is: each scenario needs the same two-engine, real-socket setup, and
// several build on state an earlier one left behind (a locked path, a
// folder to delete).
#[allow(clippy::too_many_lines)]
fn the_bridge_answers_every_i2_write_verb() {
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
    let phone = build_side_with_roots(
        "Pixel 3 XL",
        DeviceKind::Phone,
        phone_key.clone(),
        &[
            RootSpec {
                name: "Root",
                writable: true,
                files: &[("Existing.txt", b"the original bytes")],
            },
            RootSpec {
                name: "Second",
                writable: true,
                files: &[],
            },
        ],
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

    let endpoint = mac
        .engine
        .mount_start(phone_key_hex)
        .expect("mount_start should succeed for a paired, reachable device");
    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");
    let host = format!("127.0.0.1:{}", port_of(&endpoint.url));
    let auth = (endpoint.user.as_str(), endpoint.password.as_str());
    let mut client = TestClient::connect(addr);

    // A request against a path no earlier step in this test has ever
    // touched. The access log finalises a pending entry when the same
    // peer connection touches a different path (`docs/engine-contract.md`,
    // item 13, "Rolling up"), and the DAV bridge's pool keeps one
    // connection open across every request this test makes; calling this
    // after an interesting operation, before reading the log, is what
    // makes that operation's own entry visible to `access_log` at once,
    // the same trick the I1 test above uses for its cache assertions.
    let flush = |client: &mut TestClient| {
        let response = client.request(
            "PROPFIND",
            "/Second",
            &host,
            Some(auth),
            &[("Depth", "0".to_owned())],
            Some(b""),
        );
        assert_eq!(
            response.status, 207,
            "the flush request itself must succeed"
        );
    };

    // --- PUT of a new file: 201, the right bytes, and a This/Write entry.
    let new_bytes = pattern(5000);
    let response = client.request(
        "PUT",
        "/Root/New.bin",
        &host,
        Some(auth),
        &[],
        Some(&new_bytes),
    );
    assert_eq!(response.status, 201);
    let response = client.request("GET", "/Root/New.bin", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, new_bytes);
    flush(&mut client);
    assert!(
        count_entries(
            &mac.engine.access_log(None, 1000),
            ferry_runtime::Actor::This,
            ferry_runtime::AccessVerb::Write
        ) >= 1,
        "a PUT should leave a This/Write entry"
    );

    // --- PUT without Content-Length is 411.
    let mut no_length = TestClient::connect(addr);
    let credentials = base64_encode(format!("{}:{}", auth.0, auth.1).as_bytes());
    no_length.write_raw_head(&format!(
        "PUT /Root/NoLength.bin HTTP/1.1\r\nHost: {host}\r\nAuthorization: Basic {credentials}\r\n\r\n"
    ));
    let response = no_length.read_response(false);
    assert_eq!(response.status, 411);
    let response = client.request("GET", "/Root/NoLength.bin", &host, Some(auth), &[], None);
    assert_eq!(response.status, 404, "a refused PUT must not land anything");

    // --- PUT over an existing file with one changed chunk writes only
    // that chunk: `docs/engine-contract.md`, item 6, "the delta on save".
    let chunk = 1024 * 1024_usize;
    let original = pattern(chunk * 2);
    let mut changed = original.clone();
    changed[chunk] ^= 0xFF;
    let response = client.request(
        "PUT",
        "/Root/Delta.bin",
        &host,
        Some(auth),
        &[],
        Some(&original),
    );
    assert_eq!(response.status, 201);
    flush(&mut client);
    let before = sum_bytes(
        &phone.engine.access_log(None, 1000),
        ferry_runtime::Actor::Peer,
        ferry_runtime::AccessVerb::Write,
    );
    let response = client.request(
        "PUT",
        "/Root/Delta.bin",
        &host,
        Some(auth),
        &[],
        Some(&changed),
    );
    assert_eq!(response.status, 204);
    flush(&mut client);
    let after = sum_bytes(
        &phone.engine.access_log(None, 1000),
        ferry_runtime::Actor::Peer,
        ferry_runtime::AccessVerb::Write,
    );
    assert_eq!(
        after - before,
        chunk as u64,
        "only the one changed chunk should reach the peer"
    );
    let response = client.request("GET", "/Root/Delta.bin", &host, Some(auth), &[], None);
    assert_eq!(response.body, changed);

    // --- MKCOL creates, repeats as 405.
    let response = client.request("MKCOL", "/Root/Folder", &host, Some(auth), &[], None);
    assert_eq!(response.status, 201);
    let response = client.request("MKCOL", "/Root/Folder", &host, Some(auth), &[], None);
    assert_eq!(response.status, 405);
    // A missing parent is 409, not 404: RFC 4918 9.3.1.
    let response = client.request(
        "MKCOL",
        "/Root/NoSuchParent/Folder",
        &host,
        Some(auth),
        &[],
        None,
    );
    assert_eq!(response.status, 409);
    flush(&mut client);
    assert!(
        count_entries(
            &mac.engine.access_log(None, 1000),
            ferry_runtime::Actor::This,
            ferry_runtime::AccessVerb::Mkdir
        ) >= 1,
        "a MKCOL should leave a This/Mkdir entry"
    );

    // --- DELETE of a nested folder removes everything, leaves first,
    // folders deepest first, over `folder.rs`'s bounds.
    let response = client.request("MKCOL", "/Root/Folder/Sub", &host, Some(auth), &[], None);
    assert_eq!(response.status, 201);
    let response = client.request(
        "PUT",
        "/Root/Folder/a.txt",
        &host,
        Some(auth),
        &[],
        Some(b"a"),
    );
    assert_eq!(response.status, 201);
    let response = client.request(
        "PUT",
        "/Root/Folder/Sub/b.txt",
        &host,
        Some(auth),
        &[],
        Some(b"b"),
    );
    assert_eq!(response.status, 201);
    flush(&mut client);
    let deletes_before = count_entries(
        &phone.engine.access_log(None, 1000),
        ferry_runtime::Actor::Peer,
        ferry_runtime::AccessVerb::Delete,
    );
    let response = client.request("DELETE", "/Root/Folder", &host, Some(auth), &[], None);
    assert_eq!(response.status, 204);
    flush(&mut client);
    let deletes_after = count_entries(
        &phone.engine.access_log(None, 1000),
        ferry_runtime::Actor::Peer,
        ferry_runtime::AccessVerb::Delete,
    );
    assert_eq!(
        deletes_after - deletes_before,
        4,
        "the peer's log should show a.txt, Sub/b.txt, Sub, and Folder itself deleted"
    );
    let response = client.request(
        "PROPFIND",
        "/Root/Folder",
        &host,
        Some(auth),
        &[("Depth", "0".to_owned())],
        Some(b""),
    );
    assert_eq!(
        response.status, 404,
        "the whole folder should really be gone"
    );

    // --- A sidecar DELETE never reaches the peer.
    let response = client.request(
        "PUT",
        "/Root/.DS_Store",
        &host,
        Some(auth),
        &[],
        Some(b"finder bookkeeping"),
    );
    assert_eq!(response.status, 201);
    let peer_log_before = phone.engine.access_log(None, 1000).len();
    let response = client.request("DELETE", "/Root/.DS_Store", &host, Some(auth), &[], None);
    assert_eq!(response.status, 204);
    let peer_log_after = phone.engine.access_log(None, 1000).len();
    assert_eq!(
        peer_log_before, peer_log_after,
        "a sidecar DELETE must not touch the peer's access log"
    );
    let response = client.request("GET", "/Root/.DS_Store", &host, Some(auth), &[], None);
    assert_eq!(response.status, 404);

    // --- MOVE renames; `Overwrite: F` refuses with 412; cross root is 502.
    let response = client.request(
        "MOVE",
        "/Root/New.bin",
        &host,
        Some(auth),
        &[("Destination", format!("http://{host}/Root/Renamed.bin"))],
        None,
    );
    assert_eq!(response.status, 204);
    let response = client.request("GET", "/Root/Renamed.bin", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, new_bytes);
    flush(&mut client);
    assert!(
        count_entries(
            &mac.engine.access_log(None, 1000),
            ferry_runtime::Actor::This,
            ferry_runtime::AccessVerb::Rename
        ) >= 1,
        "a MOVE should leave a This/Rename entry"
    );

    let response = client.request(
        "MOVE",
        "/Root/Renamed.bin",
        &host,
        Some(auth),
        &[
            ("Destination", format!("http://{host}/Root/Existing.txt")),
            ("Overwrite", "F".to_owned()),
        ],
        None,
    );
    assert_eq!(response.status, 412);
    // Nothing should have moved: both names still answer as before.
    let response = client.request("GET", "/Root/Renamed.bin", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);

    let response = client.request(
        "MOVE",
        "/Root/Renamed.bin",
        &host,
        Some(auth),
        &[("Destination", format!("http://{host}/Second/Renamed.bin"))],
        None,
    );
    assert_eq!(response.status, 502, "a cross root MOVE is not supported");

    // --- COPY of a file duplicates it.
    let response = client.request(
        "COPY",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[(
            "Destination",
            format!("http://{host}/Root/Existing-Copy.txt"),
        )],
        None,
    );
    assert_eq!(response.status, 201);
    let original_body = client
        .request("GET", "/Root/Existing.txt", &host, Some(auth), &[], None)
        .body;
    let copy_body = client
        .request(
            "GET",
            "/Root/Existing-Copy.txt",
            &host,
            Some(auth),
            &[],
            None,
        )
        .body;
    assert_eq!(original_body, copy_body);
    // COPY of a folder is 403.
    let response = client.request(
        "COPY",
        "/Root",
        &host,
        Some(auth),
        &[("Destination", format!("http://{host}/Second/RootCopy"))],
        None,
    );
    assert_eq!(response.status, 403);

    // --- PROPPATCH sets the modified time.
    let body = b"<?xml version=\"1.0\"?><D:propertyupdate xmlns:D=\"DAV:\"><D:set><D:prop>\
<D:getlastmodified>Tue, 09 Sep 2025 12:00:00 GMT</D:getlastmodified>\
</D:prop></D:set></D:propertyupdate>";
    let response = client.request(
        "PROPPATCH",
        "/Root/Existing-Copy.txt",
        &host,
        Some(auth),
        &[],
        Some(body),
    );
    assert_eq!(response.status, 207);
    let response = client.request(
        "GET",
        "/Root/Existing-Copy.txt",
        &host,
        Some(auth),
        &[],
        None,
    );
    assert_eq!(
        response.header("last-modified"),
        Some("Tue, 09 Sep 2025 12:00:00 GMT")
    );

    // --- LOCK a path, then a PUT without its token is 423, with it
    // succeeds.
    let response = client.request(
        "LOCK",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[],
        Some(b""),
    );
    assert_eq!(response.status, 200);
    let token = response
        .header("lock-token")
        .expect("LOCK should answer with a Lock-Token header")
        .to_owned();
    let response = client.request(
        "PUT",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[],
        Some(b"locked out"),
    );
    assert_eq!(response.status, 423);
    let response = client.request(
        "PUT",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[("If", format!("(<{token}>)"))],
        Some(b"the new bytes, written with the token"),
    );
    assert_eq!(response.status, 204);
    let response = client.request("GET", "/Root/Existing.txt", &host, Some(auth), &[], None);
    assert_eq!(response.body, b"the new bytes, written with the token");

    // --- I2-4: a second LOCK of the still-unexpired lock is 423, not a
    // fresh token; UNLOCK with the wrong token is 403; the real token
    // still works.
    let response = client.request(
        "LOCK",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[],
        Some(b""),
    );
    assert_eq!(
        response.status, 423,
        "a second LOCK of an unexpired lock must not hand out a new token"
    );
    let response = client.request(
        "UNLOCK",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[(
            "Lock-Token",
            "<opaquelocktoken:0000000000000000000000000000000>".to_owned(),
        )],
        None,
    );
    assert_eq!(
        response.status, 403,
        "UNLOCK with a token that does not match the lock is 403"
    );
    let response = client.request(
        "UNLOCK",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[("Lock-Token", format!("<{token}>"))],
        None,
    );
    assert_eq!(response.status, 204);

    // --- I2-5: a locked child refuses the whole folder DELETE, with
    // nothing removed; MOVE onto a locked destination is 423.
    let response = client.request("MKCOL", "/Root/Guarded", &host, Some(auth), &[], None);
    assert_eq!(response.status, 201);
    let response = client.request(
        "PUT",
        "/Root/Guarded/Child.txt",
        &host,
        Some(auth),
        &[],
        Some(b"guarded"),
    );
    assert_eq!(response.status, 201);
    let response = client.request(
        "LOCK",
        "/Root/Guarded/Child.txt",
        &host,
        Some(auth),
        &[],
        Some(b""),
    );
    assert_eq!(response.status, 200);
    let child_token = response
        .header("lock-token")
        .expect("LOCK should answer with a Lock-Token header")
        .to_owned();
    let response = client.request("DELETE", "/Root/Guarded", &host, Some(auth), &[], None);
    assert_eq!(
        response.status, 423,
        "a locked child must refuse the whole DELETE"
    );
    let response = client.request(
        "PROPFIND",
        "/Root/Guarded/Child.txt",
        &host,
        Some(auth),
        &[("Depth", "0".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207, "nothing should have been deleted");
    let response = client.request(
        "UNLOCK",
        "/Root/Guarded/Child.txt",
        &host,
        Some(auth),
        &[("Lock-Token", format!("<{child_token}>"))],
        None,
    );
    assert_eq!(response.status, 204);
    let response = client.request("DELETE", "/Root/Guarded", &host, Some(auth), &[], None);
    assert_eq!(
        response.status, 204,
        "the folder deletes cleanly once nothing inside it is locked"
    );

    let response = client.request(
        "PUT",
        "/Root/MoveSource.txt",
        &host,
        Some(auth),
        &[],
        Some(b"move me"),
    );
    assert_eq!(response.status, 201);
    let response = client.request(
        "LOCK",
        "/Root/LockedDestination.txt",
        &host,
        Some(auth),
        &[],
        Some(b""),
    );
    assert_eq!(response.status, 200);
    let dest_token = response
        .header("lock-token")
        .expect("LOCK should answer with a Lock-Token header")
        .to_owned();
    let response = client.request(
        "MOVE",
        "/Root/MoveSource.txt",
        &host,
        Some(auth),
        &[(
            "Destination",
            format!("http://{host}/Root/LockedDestination.txt"),
        )],
        None,
    );
    assert_eq!(
        response.status, 423,
        "a locked destination must refuse the MOVE"
    );
    let response = client.request(
        "UNLOCK",
        "/Root/LockedDestination.txt",
        &host,
        Some(auth),
        &[("Lock-Token", format!("<{dest_token}>"))],
        None,
    );
    assert_eq!(response.status, 204);

    // --- I2-6: a modified time PROPPATCH names goes to `refused` (403 in
    // the multistatus body) when the date fails to parse, not `accepted`.
    let bad_date_body =
        b"<?xml version=\"1.0\"?><D:propertyupdate xmlns:D=\"DAV:\"><D:set><D:prop>\
<D:getlastmodified>not a date</D:getlastmodified>\
</D:prop></D:set></D:propertyupdate>";
    let response = client.request(
        "PROPPATCH",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[],
        Some(bad_date_body),
    );
    assert_eq!(response.status, 207);
    assert!(
        String::from_utf8_lossy(&response.body).contains("403"),
        "an unparsable date must refuse the property instead of accepting it"
    );

    // --- I2-7: MOVE refuses a `.ferry-part` name at either end, and a
    // destination that is a probe name while the source is not.
    let response = client.request(
        "MOVE",
        "/Root/Existing.txt.ferry-part",
        &host,
        Some(auth),
        &[("Destination", format!("http://{host}/Root/WontLand.bin"))],
        None,
    );
    assert_eq!(response.status, 403, "a `.ferry-part` source is refused");
    let response = client.request(
        "MOVE",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[(
            "Destination",
            format!("http://{host}/Root/Existing.txt.ferry-part"),
        )],
        None,
    );
    assert_eq!(
        response.status, 403,
        "a `.ferry-part` destination is refused"
    );
    let response = client.request(
        "PUT",
        "/Root/WillBecomeAProbe.txt",
        &host,
        Some(auth),
        &[],
        Some(b"a real file"),
    );
    assert_eq!(response.status, 201);
    let response = client.request(
        "MOVE",
        "/Root/WillBecomeAProbe.txt",
        &host,
        Some(auth),
        &[("Destination", format!("http://{host}/Root/.DS_Store"))],
        None,
    );
    assert_eq!(
        response.status, 403,
        "a real file must not become a probe name"
    );

    // --- I2-8: COPY honours `Overwrite: F` the same way MOVE does, and a
    // probe-name source is served from the sidecar store, never the peer.
    let response = client.request(
        "COPY",
        "/Root/Existing.txt",
        &host,
        Some(auth),
        &[
            (
                "Destination",
                format!("http://{host}/Root/Existing-Copy.txt"),
            ),
            ("Overwrite", "F".to_owned()),
        ],
        None,
    );
    assert_eq!(
        response.status, 412,
        "Existing-Copy.txt already exists from the earlier COPY"
    );
    let response = client.request(
        "PUT",
        "/Root/._probe_src",
        &host,
        Some(auth),
        &[],
        Some(b"sidecar bytes"),
    );
    assert_eq!(response.status, 201);
    flush(&mut client);
    let peer_log_before = phone.engine.access_log(None, 1000).len();
    let response = client.request(
        "COPY",
        "/Root/._probe_src",
        &host,
        Some(auth),
        &[("Destination", format!("http://{host}/Root/._probe_dst"))],
        None,
    );
    assert_eq!(response.status, 201);
    flush(&mut client);
    let peer_log_after = phone.engine.access_log(None, 1000).len();
    assert_eq!(
        peer_log_before, peer_log_after,
        "a probe-name COPY must never reach the peer"
    );
    let response = client.request("GET", "/Root/._probe_dst", &host, Some(auth), &[], None);
    assert_eq!(response.body, b"sidecar bytes");

    // --- A `.ferry-part` never shows in a listing, and a `GET` or `HEAD`
    // of one is 404, even when a real one sits on the peer's disk: this
    // writes straight to the phone's underlying folder, bypassing the
    // bridge entirely, the way a partial genuinely left behind by a
    // connection lost mid landing would.
    std::fs::write(
        phone
            .shared
            .path()
            .join("Root")
            .join("Stray.bin.ferry-part"),
        b"a partial another attempt never finished",
    )
    .expect("writing a stray partial directly to disk should succeed");
    let response = client.request(
        "PROPFIND",
        "/Root",
        &host,
        Some(auth),
        &[("Depth", "1".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    assert!(
        !String::from_utf8_lossy(&response.body).contains("ferry-part"),
        "a `.ferry-part` on disk must never show in a listing"
    );
    let response = client.request(
        "GET",
        "/Root/Stray.bin.ferry-part",
        &host,
        Some(auth),
        &[],
        None,
    );
    assert_eq!(response.status, 404);
    let response = client.request(
        "HEAD",
        "/Root/Stray.bin.ferry-part",
        &host,
        Some(auth),
        &[],
        None,
    );
    assert_eq!(response.status, 404);

    // --- A PUT the peer refuses leaves no spool file behind. A second,
    // fresh pair: the phone's one root is read only, seeded before either
    // engine starts, the same way `mac` and `phone` above are paired.
    let second_mac_key = generate_key().expect("a fresh key pair");
    let read_only_key = generate_key().expect("a fresh key pair");
    let second_mac = build_side(
        "Vamana Two",
        DeviceKind::Mac,
        second_mac_key.clone(),
        &[],
        &read_only_key,
        "Read Only Pixel",
        DeviceKind::Phone,
    );
    let read_only_phone = build_side_with_roots(
        "Read Only Pixel",
        DeviceKind::Phone,
        read_only_key,
        &[RootSpec {
            name: "Root",
            writable: false,
            files: &[],
        }],
        &second_mac_key,
        "Vamana Two",
        DeviceKind::Mac,
    );
    read_only_phone.engine.set_reachable(true);
    let read_only_key_hex = second_mac
        .engine
        .devices()
        .first()
        .expect("the read only phone should already be paired")
        .key_hex
        .clone();
    second_mac
        .engine
        .offer_candidate(loopback_addr(&read_only_phone));
    second_mac
        .engine
        .list(read_only_key_hex.clone(), String::new())
        .expect("listing the read only phone should succeed once dialable");
    let read_only_endpoint = second_mac
        .engine
        .mount_start(read_only_key_hex.clone())
        .expect("mount_start on the read only phone");
    let read_only_addr: SocketAddr = format!("127.0.0.1:{}", port_of(&read_only_endpoint.url))
        .parse()
        .expect("a loopback address");
    let read_only_host = format!("127.0.0.1:{}", port_of(&read_only_endpoint.url));
    let read_only_auth = (
        read_only_endpoint.user.as_str(),
        read_only_endpoint.password.as_str(),
    );
    let mut read_only_client = TestClient::connect(read_only_addr);
    let response = read_only_client.request(
        "PUT",
        "/Root/Refused.bin",
        &read_only_host,
        Some(read_only_auth),
        &[],
        Some(&pattern(1000)),
    );
    assert!(
        matches!(response.status, 403 | 502 | 503),
        "a PUT the peer's read only root refuses should answer with a write refusal, got {}",
        response.status
    );
    // `docs/engine-contract.md`, item 6: "a failed landing removes the
    // spool file." This device has never had a `PUT` land, so its
    // spool folder holds nothing but what the refused attempt above left
    // behind, which should be nothing at all.
    let spool_dir = second_mac
        .data
        .path()
        .join("dav_spool")
        .join(&read_only_key_hex);
    let remaining = std::fs::read_dir(&spool_dir)
        .map(Iterator::count)
        .unwrap_or(0);
    assert_eq!(
        remaining, 0,
        "a PUT the peer refuses must leave no spool file behind"
    );
    read_only_phone.engine.stop();
    second_mac.engine.stop();

    mac.engine.stop();
    phone.engine.stop();
}
