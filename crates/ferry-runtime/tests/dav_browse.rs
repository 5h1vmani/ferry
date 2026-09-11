//! The `WebDAV` bridge browsing a device, `docs/engine-contract.md`, item
//! 6, I1.
//!
//! A hand-written HTTP client over `TcpStream`, against two paired engines
//! in one process: one (`mac`) mounts the other's (`phone`) shared root and
//! browses it through the bridge; the other serves real files from a real
//! folder, so every answer this test checks came from a real round trip,
//! not a mock.
//!
//! One long narrative test covers every I1 verb, the cache, the lock table,
//! auth and `Host` refusals, and probes never reaching the peer, in the
//! order a mount's life actually runs in. Splitting every scenario into its
//! own test would mean building two engines and dialing a real socket for
//! each one; this file pays that cost once.
//!
//! The engines, the folders, and the HTTP client this file drives live in
//! `tests/common/mod.rs`, so `tests/item_17_prefetch.rs` can drive the same
//! bridge without a second copy of them.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use ferry_runtime::{DeviceKind, generate_key};

use common::{PATIENCE, TestClient, base64_encode, build_side, loopback_addr, pattern, port_of};

#[test]
// One long narrative, in the order a mount's life actually runs: start,
// browse every verb, then the cases that only apply once the peer or the
// bridge itself has gone. Splitting this into one test per verb would
// rebuild two engines and redial a real socket for each one.
#[allow(clippy::too_many_lines)]
fn the_bridge_serves_a_devices_files_and_answers_every_i1_verb() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");

    let notes = b"hello from the phone".to_vec();
    let big = pattern(2 * 1024 * 1024 + 777);
    let photo = b"not really a jpeg".to_vec();
    // Comfortably past any OS socket buffer this test's own connection
    // will use, so a client that stops reading after a small prefix is
    // guaranteed to leave the bridge blocked mid-transfer rather than
    // having already written the whole file into the buffer unread.
    let huge = pattern(16 * 1024 * 1024);

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
        &[
            ("Notes.txt", notes.as_slice()),
            ("Big.bin", big.as_slice()),
            ("DCIM/IMG_0001.jpg", photo.as_slice()),
            ("Huge.bin", huge.as_slice()),
        ],
        &mac_key,
        "Vamana",
        DeviceKind::Mac,
    );
    // Only a reachable device accepts inbound connections at all
    // (`Engine::set_reachable`); without this the Mac's dial is dropped at
    // accept and never reaches a handshake.
    phone.engine.set_reachable(true);

    let phone_key_hex = mac
        .engine
        .devices()
        .first()
        .expect("the phone should already be paired, from the seeded peer store")
        .key_hex
        .clone();

    // A real Mac only calls `mount_start` once it already knows the phone
    // is reachable, which it learns from a connection it already made. This
    // `list` call is that connection: it also injects the phone's loopback
    // address as a dial target, since discovery is not running in a test.
    mac.engine.offer_candidate(loopback_addr(&phone));
    mac.engine
        .list(phone_key_hex.clone(), String::new())
        .expect("listing the phone's root should succeed once dialable");

    let endpoint = mac
        .engine
        .mount_start(phone_key_hex.clone())
        .expect("mount_start should succeed for a paired, reachable device");
    assert_eq!(
        endpoint.url,
        format!("http://127.0.0.1:{}/", port_of(&endpoint.url))
    );

    let again = mac
        .engine
        .mount_start(phone_key_hex.clone())
        .expect("a second mount_start should succeed");
    assert_eq!(again.url, endpoint.url, "mount_start is idempotent");
    assert_eq!(
        again.password, endpoint.password,
        "the same bridge, the same password"
    );

    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");
    let host = format!("127.0.0.1:{}", port_of(&endpoint.url));
    let auth = (endpoint.user.as_str(), endpoint.password.as_str());

    let mut client = TestClient::connect(addr);

    // OPTIONS.
    let response = client.request("OPTIONS", "/", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);
    assert!(
        response
            .header("allow")
            .is_some_and(|allow| allow.contains("PROPFIND"))
    );

    // Wrong Host: 400, before auth is even considered.
    let mut bad_host = TestClient::connect(addr);
    let response = bad_host.request("OPTIONS", "/", "example.com", Some(auth), &[], None);
    assert_eq!(
        response.status, 400,
        "a Host that is not 127.0.0.1:<port> is refused"
    );

    // Wrong password: 401, with a challenge.
    let mut bad_auth = TestClient::connect(addr);
    let response = bad_auth.request(
        "OPTIONS",
        "/",
        &host,
        Some((endpoint.user.as_str(), "not-it")),
        &[],
        None,
    );
    assert_eq!(response.status, 401);
    assert!(response.header("www-authenticate").is_some());

    // B1: a `Content-Length` past the allocation bound is refused before
    // a single byte of the (never sent) body is read, so this answers at
    // once rather than waiting on bytes that never arrive.
    let mut oversized_body = TestClient::connect(addr);
    let credentials = base64_encode(format!("{}:{}", auth.0, auth.1).as_bytes());
    oversized_body.write_raw_head(&format!(
        "GET /Root/Notes.txt HTTP/1.1\r\nHost: {host}\r\nAuthorization: Basic {credentials}\r\nContent-Length: 10000000\r\n\r\n"
    ));
    let response = oversized_body.read_response(false);
    assert_eq!(response.status, 413);

    // B2: a single header line far past the 64 KiB head budget is
    // refused, rather than grown without bound.
    let mut long_header = TestClient::connect(addr);
    let mut head = format!("GET /Root/Notes.txt HTTP/1.1\r\nHost: {host}\r\n");
    head.push_str("X-Filler: ");
    head.push_str(&"a".repeat(100 * 1024));
    head.push_str("\r\n\r\n");
    long_header.write_raw_head(&head);
    let response = long_header.read_response(false);
    assert_eq!(response.status, 431);

    // PROPFIND depth 1 of the root shows the peer's one root, "Root".
    let response = client.request(
        "PROPFIND",
        "/",
        &host,
        Some(auth),
        &[("Depth", "1".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    let body = String::from_utf8_lossy(&response.body).into_owned();
    assert!(body.contains("<D:href>/Root/</D:href>"), "{body}");
    assert!(
        body.contains("<D:displayname>Root</D:displayname>"),
        "{body}"
    );
    assert!(
        body.contains("<D:collection/>"),
        "Root is listed as a collection: {body}"
    );
    // N4: the mount root's own entry (href "/") is named after the
    // device, not left blank.
    assert!(
        body.contains("<D:href>/</D:href>")
            && body.contains("<D:displayname>Pixel 3 XL</D:displayname>"),
        "the mount root should show the device's name: {body}"
    );

    // N3, RFC 4918 9.1: a `PROPFIND` with no `Depth` header, or an
    // explicit `infinity`, is refused. I1 never walks a whole tree.
    let response = client.request("PROPFIND", "/Root", &host, Some(auth), &[], Some(b""));
    assert_eq!(response.status, 403);
    let response = client.request(
        "PROPFIND",
        "/Root",
        &host,
        Some(auth),
        &[("Depth", "infinity".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 403);

    // N3: a method this bridge never answers is 405, with an Allow header
    // naming what it does answer.
    let response = client.request("TRACE", "/Root/Notes.txt", &host, Some(auth), &[], None);
    assert_eq!(response.status, 405);
    assert!(
        response
            .header("allow")
            .is_some_and(|allow| allow.contains("PROPFIND"))
    );

    // PROPFIND depth 1 of a folder shows its entries with the six
    // properties, whatever the request body asks for: an empty body means
    // every property.
    let response = client.request(
        "PROPFIND",
        "/Root",
        &host,
        Some(auth),
        &[("Depth", "1".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    let body = String::from_utf8_lossy(&response.body).into_owned();
    for expected in [
        "<D:href>/Root/Notes.txt</D:href>",
        "<D:href>/Root/Big.bin</D:href>",
        "<D:href>/Root/DCIM/</D:href>",
        "resourcetype",
        "getcontentlength",
        "getlastmodified",
        "getetag",
        "creationdate",
        "displayname",
    ] {
        assert!(body.contains(expected), "missing {expected} in {body}");
    }

    // PROPFIND depth 0 of a file: one response, not a collection.
    let response = client.request(
        "PROPFIND",
        "/Root/Notes.txt",
        &host,
        Some(auth),
        &[("Depth", "0".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    let body = String::from_utf8_lossy(&response.body).into_owned();
    assert_eq!(body.matches("<D:response>").count(), 1, "{body}");
    assert!(!body.contains("<D:collection/>"), "{body}");

    // GET, whole file.
    let response = client.request("GET", "/Root/Notes.txt", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, notes);

    // S3: a bridge GET and PROPFIND each leave a `This` entry in the
    // Mac's own access log, with the right verb, once the peer round
    // trip they needed is done.
    let this_entries = |log: &[ferry_runtime::AccessEntry], verb: ferry_runtime::AccessVerb| {
        log.iter()
            .filter(|entry| entry.actor == ferry_runtime::Actor::This && entry.verb == verb)
            .count()
    };
    let log = mac.engine.access_log(None, 1000);
    assert!(
        this_entries(&log, ferry_runtime::AccessVerb::Read) >= 1,
        "a bridge GET should leave a This/Read entry"
    );
    assert!(
        this_entries(&log, ferry_runtime::AccessVerb::List) >= 1,
        "a bridge PROPFIND of a folder should leave a This/List entry"
    );
    assert!(
        this_entries(&log, ferry_runtime::AccessVerb::Stat) >= 1,
        "a bridge PROPFIND at depth 0 should leave a This/Stat entry"
    );

    // S1: a range past the end, and an empty suffix, both answer 416 with
    // the right Content-Range, never a 200 with the wrong length.
    let response = client.request(
        "GET",
        "/Root/Notes.txt",
        &host,
        Some(auth),
        &[("Range", "bytes=1000-2000".to_owned())],
        None,
    );
    assert_eq!(response.status, 416);
    assert_eq!(
        response.header("content-range"),
        Some(format!("bytes */{}", notes.len()).as_str())
    );
    let response = client.request(
        "GET",
        "/Root/Notes.txt",
        &host,
        Some(auth),
        &[("Range", "bytes=-0".to_owned())],
        None,
    );
    assert_eq!(response.status, 416);
    assert_eq!(
        response.header("content-range"),
        Some(format!("bytes */{}", notes.len()).as_str())
    );

    // GET with Range, and served in pieces, since the file is bigger than
    // one mebibyte: this exercises more than one `read` on the peer.
    let start = 1_000_000usize;
    let end = 1_200_000usize; // inclusive on the wire
    let response = client.request(
        "GET",
        "/Root/Big.bin",
        &host,
        Some(auth),
        &[("Range", format!("bytes={start}-{end}"))],
        None,
    );
    assert_eq!(response.status, 206);
    assert_eq!(response.body, big[start..=end]);
    assert_eq!(
        response.header("content-range"),
        Some(format!("bytes {start}-{end}/{}", big.len()).as_str())
    );

    // HEAD: the same headers, no body.
    let response = client.request("HEAD", "/Root/Notes.txt", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-length"),
        Some(notes.len().to_string().as_str())
    );
    assert!(response.body.is_empty());

    // A probe read is 404, and never reaches the peer.
    let before = phone.engine.access_log(None, 1000).len();
    let response = client.request("GET", "/Root/.DS_Store", &host, Some(auth), &[], None);
    assert_eq!(response.status, 404);
    let after = phone.engine.access_log(None, 1000).len();
    assert_eq!(
        before, after,
        "a probe read must not touch the peer's access log"
    );

    // A PUT of a sidecar name is accepted and served back, and the peer
    // still never sees it.
    let sidecar = b"finder's own bookkeeping";
    let response = client.request(
        "PUT",
        "/Root/.DS_Store",
        &host,
        Some(auth),
        &[],
        Some(sidecar),
    );
    assert_eq!(response.status, 201);
    let response = client.request("GET", "/Root/.DS_Store", &host, Some(auth), &[], None);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, sidecar);
    let after_put = phone.engine.access_log(None, 1000).len();
    assert_eq!(
        before, after_put,
        "a sidecar PUT must not touch the peer's access log"
    );

    // S4: a sidecar body over the 64 KiB size bound is 413, and never
    // lands on disk.
    let oversized_sidecar = vec![0u8; 64 * 1024 + 1];
    let response = client.request(
        "PUT",
        "/Root/._oversized",
        &host,
        Some(auth),
        &[],
        Some(&oversized_sidecar),
    );
    assert_eq!(response.status, 413);
    let response = client.request("GET", "/Root/._oversized", &host, Some(auth), &[], None);
    assert_eq!(
        response.status, 404,
        "a refused sidecar body must not land on disk"
    );

    // Two sidecars sharing a name under different folders stay separate:
    // each is keyed by its own DAV path, not the bare name alone.
    let sidecar_root = b"folder Root's own bookkeeping";
    let sidecar_dcim = b"folder DCIM's own bookkeeping, different bytes";
    let response = client.request(
        "PUT",
        "/Root/._twin",
        &host,
        Some(auth),
        &[],
        Some(sidecar_root),
    );
    assert_eq!(response.status, 201);
    let response = client.request(
        "PUT",
        "/Root/DCIM/._twin",
        &host,
        Some(auth),
        &[],
        Some(sidecar_dcim),
    );
    assert_eq!(response.status, 201);
    let response = client.request("GET", "/Root/._twin", &host, Some(auth), &[], None);
    assert_eq!(response.body, sidecar_root);
    let response = client.request("GET", "/Root/DCIM/._twin", &host, Some(auth), &[], None);
    assert_eq!(response.body, sidecar_dcim);

    // The cache: two depth 1 listings of an untouched folder within two
    // seconds cost the peer exactly one `list`. A folder never listed
    // before in this test, so no earlier cache entry can make this
    // ambiguous.
    let list_count = |log: &[ferry_runtime::AccessEntry]| {
        log.iter()
            .filter(|entry| {
                entry.actor == ferry_runtime::Actor::Peer
                    && entry.verb == ferry_runtime::AccessVerb::List
            })
            .count()
    };
    let before = list_count(&phone.engine.access_log(None, 1000));
    for _ in 0..2 {
        let response = client.request(
            "PROPFIND",
            "/Root/DCIM",
            &host,
            Some(auth),
            &[("Depth", "1".to_owned())],
            Some(b""),
        );
        assert_eq!(response.status, 207);
    }
    // The access log finalises an entry when its connection touches a
    // different path, not on every call (`docs/engine-contract.md`, item
    // 13): the pool keeps this connection open, so the "DCIM" list entry
    // stays pending until something else is asked of it. This stat does
    // that, so the count below reflects what already happened above rather
    // than waiting five seconds for the idle rule to finalise it.
    let response = client.request(
        "PROPFIND",
        "/Root/Notes.txt",
        &host,
        Some(auth),
        &[("Depth", "0".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    let after = list_count(&phone.engine.access_log(None, 1000));
    assert_eq!(
        after - before,
        1,
        "the second listing within two seconds must hit the cache"
    );

    // The cache expires after two seconds: a listing made just past that
    // window costs the peer another `list`, one sleep rather than a test
    // clock.
    std::thread::sleep(Duration::from_millis(2100));
    let before_expiry = list_count(&phone.engine.access_log(None, 1000));
    let response = client.request(
        "PROPFIND",
        "/Root/DCIM",
        &host,
        Some(auth),
        &[("Depth", "1".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    // As above: this new "DCIM" list entry stays pending on the peer's
    // connection until it touches a different path, so one more request
    // elsewhere finalises it before the count below is read.
    let response = client.request(
        "PROPFIND",
        "/Root/Notes.txt",
        &host,
        Some(auth),
        &[("Depth", "0".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 207);
    let after_expiry = list_count(&phone.engine.access_log(None, 1000));
    assert_eq!(
        after_expiry - before_expiry,
        1,
        "a listing past the two second window must ask the peer again"
    );

    // LOCK returns a token; UNLOCK with it succeeds.
    let response = client.request("LOCK", "/Root/.DS_Store", &host, Some(auth), &[], Some(b""));
    assert_eq!(response.status, 200);
    let token = response
        .header("lock-token")
        .expect("LOCK should answer with a Lock-Token header")
        .to_owned();
    let response = client.request(
        "UNLOCK",
        "/Root/.DS_Store",
        &host,
        Some(auth),
        &[("Lock-Token", token)],
        None,
    );
    assert_eq!(response.status, 204);

    // I2 builds the write verbs; exhaustive coverage of every one lives
    // in `the_bridge_answers_every_i2_write_verb`, below. This is a
    // narrow smoke test that a real `DELETE` reaches the peer on this
    // same mount, now that I1's blanket 403 is gone.
    let response = client.request("DELETE", "/Root/Notes.txt", &host, Some(auth), &[], None);
    assert_eq!(response.status, 204);
    let response = client.request("GET", "/Root/Notes.txt", &host, Some(auth), &[], None);
    assert_eq!(response.status, 404, "Notes.txt should really be gone");

    // S2: a peer failing mid body ends the connection, since the
    // declared `Content-Length` can no longer be met. A raw connection
    // reads the headers and a small prefix of "Huge.bin" (16 MiB, far
    // past any OS socket buffer this test leaves undrained), so the
    // bridge is left blocked writing pieces this connection has not
    // read. The phone stopping there makes its next `read` RPC call
    // fail before the rest of the file ever arrives.
    let mut raw = TcpStream::connect(addr).expect("a raw connection for the partial read");
    raw.set_read_timeout(Some(PATIENCE))
        .expect("a read timeout should set");
    let credentials = base64_encode(format!("{}:{}", auth.0, auth.1).as_bytes());
    write!(
        raw,
        "GET /Root/Huge.bin HTTP/1.1\r\nHost: {host}\r\nAuthorization: Basic {credentials}\r\n\r\n"
    )
    .expect("the raw request should write");
    let mut raw_reader = BufReader::new(raw.try_clone().expect("the raw stream should clone"));
    let mut status_line = String::new();
    raw_reader
        .read_line(&mut status_line)
        .expect("a status line should arrive");
    assert!(status_line.contains("200"), "{status_line}");
    let mut declared_len = 0usize;
    loop {
        let mut line = String::new();
        raw_reader
            .read_line(&mut line)
            .expect("a header line should arrive");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.trim().eq_ignore_ascii_case("content-length")
        {
            declared_len = value.trim().parse().expect("a numeric Content-Length");
        }
    }
    assert_eq!(declared_len, huge.len());

    let mut prefix = vec![0u8; 4096];
    let first = raw_reader
        .read(&mut prefix)
        .expect("a first slice of the body should arrive");
    assert!(first > 0);
    let mut received = first;

    phone.engine.stop();

    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match raw_reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => received += count,
        }
    }
    assert!(
        received < declared_len,
        "a peer failing mid body must not deliver the whole declared length: got {received} of {declared_len}"
    );

    // PROPFIND on the existing connection answers 503 within one second,
    // whether the bridge reuses a pooled connection the peer now refuses
    // everything on, or dials fresh into a port nothing listens on anymore.
    let started = Instant::now();
    let response = client.request(
        "PROPFIND",
        "/",
        &host,
        Some(auth),
        &[("Depth", "0".to_owned())],
        Some(b""),
    );
    assert_eq!(response.status, 503);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "took {:?} to answer 503",
        started.elapsed()
    );

    mac.engine.stop();
}
