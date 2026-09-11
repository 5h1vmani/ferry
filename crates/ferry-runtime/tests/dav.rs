//! The `WebDAV` bridge, `docs/engine-contract.md`, item 6, I1.
//!
//! A hand-written HTTP client over `TcpStream`, against two paired engines
//! in one process: one (`mac`) mounts the other's (`phone`) shared root and
//! browses it through the bridge; the other serves real files from a real
//! folder, so every answer this test checks came from a real round trip,
//! not a mock.
//!
//! One long narrative test covers every verb, the cache, the lock table,
//! auth and `Host` refusals, and probes never reaching the peer, in the
//! order a mount's life actually runs in. Two small tests at the end cover
//! what only makes sense once that life is over: the peer engine stopping,
//! and `mount_stop` itself. Splitting every scenario into its own test
//! would mean building two engines and dialing a real socket for each one;
//! this file pays that cost once for everything that does not require the
//! peer or the bridge to already be down.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use ferry_core::noise::PublicKey;
use ferry_core::peers::{DeviceKind as CoreDeviceKind, Peer, PeerStore};
use ferry_runtime::{
    Config, DeviceKind, Engine, EngineListener, KeyPair, PairingState, Root, generate_key,
};

/// How long any wait may take before the test gives up.
const PATIENCE: Duration = Duration::from_secs(10);

/// A listener that does nothing. This file dials real sockets and reads
/// real answers; it never needs a change notification.
struct Silent;

impl EngineListener for Silent {
    fn devices_changed(&self) {}
    fn transfers_changed(&self) {}
    fn pairing_changed(&self, _state: PairingState) {}
    fn access_log_changed(&self) {}
}

/// One engine and the folders it owns, kept alive for the life of the
/// test.
struct Side {
    engine: std::sync::Arc<Engine>,
    _data: tempfile::TempDir,
    _shared: tempfile::TempDir,
    _download: tempfile::TempDir,
}

fn public_key_of(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

fn core_kind(kind: DeviceKind) -> CoreDeviceKind {
    match kind {
        DeviceKind::Phone => CoreDeviceKind::Phone,
        DeviceKind::Mac => CoreDeviceKind::Mac,
    }
}

/// Writes a peer store that already holds the other side, so the two
/// engines recognise each other without running the pairing handshake:
/// this file tests the bridge, not pairing, which `two_engines.rs` already
/// covers.
fn seed_peer(
    data_dir: &std::path::Path,
    peer_key: &KeyPair,
    peer_name: &str,
    peer_kind: DeviceKind,
) {
    let mut peers = PeerStore::load(&data_dir.join("peers.bin"), core_kind(peer_kind))
        .expect("a missing peer store loads empty");
    peers.add(Peer {
        key: public_key_of(peer_key),
        name: peer_name.to_owned(),
        paired_unix_secs: 0,
        kind: core_kind(peer_kind),
    });
    peers.save().expect("the seeded peer store should save");
}

#[allow(clippy::too_many_arguments)]
fn build_side(
    name: &str,
    kind: DeviceKind,
    own_key: KeyPair,
    files: &[(&str, &[u8])],
    peer_key: &KeyPair,
    peer_name: &str,
    peer_kind: DeviceKind,
) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    for (relative, contents) in files {
        let path = shared.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a folder for a fixture file");
        }
        std::fs::write(path, contents).expect("a fixture file should write");
    }
    seed_peer(data.path(), peer_key, peer_name, peer_kind);

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
        key: own_key,
        kind,
    };
    let engine = Engine::new(config, Box::new(Silent)).expect("the engine should build");
    engine.start().expect("the engine should start");
    Side {
        engine,
        _data: data,
        _shared: shared,
        _download: download,
    }
}

fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        bound.port(),
    )
}

/// The port named in a `MountEndpoint.url`, which is always
/// `"http://127.0.0.1:<port>/"`.
fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix('/'))
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("unexpected mount url shape: {url}"))
}

/// Standard base64, the encoding half of what `dav::http::base64_decode`
/// reads. Written out for the same reason the bridge itself writes its own:
/// no base64 crate is a dependency of this workspace.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[usize::from(b0 >> 2)] as char);
        out.push(ALPHABET[usize::from(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4))] as char);
        out.push(match b1 {
            Some(b1) => ALPHABET[usize::from(((b1 & 0x0F) << 2) | (b2.unwrap_or(0) >> 6))] as char,
            None => '=',
        });
        out.push(match b2 {
            Some(b2) => ALPHABET[usize::from(b2 & 0x3F)] as char,
            None => '=',
        });
    }
    out
}

/// One answer from the bridge.
struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// A hand-written HTTP/1.1 client over one `TcpStream`, kept open across
/// requests the way Finder keeps its own connection open.
struct TestClient {
    write_half: TcpStream,
    reader: BufReader<TcpStream>,
}

impl TestClient {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("the bridge should accept a connection");
        stream
            .set_read_timeout(Some(PATIENCE))
            .expect("a read timeout should set");
        let reader = BufReader::new(stream.try_clone().expect("the stream should clone"));
        Self {
            write_half: stream,
            reader,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn request(
        &mut self,
        method: &str,
        path: &str,
        host: &str,
        auth: Option<(&str, &str)>,
        extra: &[(&str, String)],
        body: Option<&[u8]>,
    ) -> Response {
        use std::fmt::Write as _;
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
        if let Some((user, password)) = auth {
            let credentials = base64_encode(format!("{user}:{password}").as_bytes());
            let _ = write!(head, "Authorization: Basic {credentials}\r\n");
        }
        for (key, value) in extra {
            let _ = write!(head, "{key}: {value}\r\n");
        }
        if let Some(body) = body {
            let _ = write!(head, "Content-Length: {}\r\n", body.len());
        }
        head.push_str("\r\n");
        self.write_half
            .write_all(head.as_bytes())
            .expect("the request head should write");
        if let Some(body) = body {
            self.write_half
                .write_all(body)
                .expect("the request body should write");
        }
        // A HEAD response states the Content-Length a GET would carry, but
        // never sends a body: reading one back would block forever.
        self.read_response(method == "HEAD")
    }

    /// Writes `head` exactly as given, with no computed `Content-Length`
    /// and no body of its own. `request` cannot express a header that
    /// lies about the body size, or one line far past any legitimate
    /// header, since it always writes a `Content-Length` matching a real
    /// body it also sends.
    fn write_raw_head(&mut self, head: &str) {
        self.write_half
            .write_all(head.as_bytes())
            .expect("a hand-built request head should write");
    }

    fn read_response(&mut self, no_body: bool) -> Response {
        let mut status_line = String::new();
        self.reader
            .read_line(&mut status_line)
            .expect("a status line should arrive");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .expect("a status line has a code")
            .parse()
            .expect("the status code should be a number");

        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .expect("a header line should arrive");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }

        let len: usize = if no_body {
            0
        } else {
            headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        };
        let mut body = vec![0u8; len];
        if len > 0 {
            self.reader
                .read_exact(&mut body)
                .expect("the body should arrive whole");
        }
        Response {
            status,
            headers,
            body,
        }
    }
}

/// Deterministic bytes, so a range read can be checked byte for byte.
fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect()
}

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

    // Any other write verb is 403 in I1.
    let response = client.request("DELETE", "/Root/Notes.txt", &host, Some(auth), &[], None);
    assert_eq!(response.status, 403);

    // Once the peer engine stops, PROPFIND answers 503 within one second,
    // whether the bridge reuses a pooled connection the peer now refuses
    // everything on, or dials fresh into a port nothing listens on anymore.
    phone.engine.stop();
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

#[test]
fn mount_stop_closes_the_port() {
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

    let endpoint = mac
        .engine
        .mount_start(phone_key_hex.clone())
        .expect("mount_start");
    let port = port_of(&endpoint.url);
    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("a loopback address");
    // The port answers before the stop.
    drop(TcpStream::connect(addr).expect("the bridge should be listening"));

    mac.engine.mount_stop(phone_key_hex);
    assert!(
        TcpStream::connect(addr).is_err(),
        "the port must be closed once mount_stop returns"
    );

    mac.engine.stop();
    phone.engine.stop();
}

#[test]
fn mount_start_on_an_unknown_device_is_not_paired() {
    let mac = build_side(
        "Vamana",
        DeviceKind::Mac,
        generate_key().expect("a fresh key pair"),
        &[],
        &generate_key().expect("a fresh key pair"),
        "Nobody",
        DeviceKind::Phone,
    );
    let error = mac
        .engine
        .mount_start("not-a-real-key-hex".to_owned())
        .expect_err("an unknown device cannot be mounted");
    let ferry_runtime::FerryError::Failed { code, .. } = error;
    assert_eq!(code, "Runtime::NotPaired");
    mac.engine.stop();
}

#[test]
fn set_mount_path_is_read_back_through_device_info() {
    let mac_key = generate_key().expect("a fresh key pair");
    let phone_key = generate_key().expect("a fresh key pair");
    let mac = build_side(
        "Vamana",
        DeviceKind::Mac,
        mac_key,
        &[],
        &phone_key,
        "Pixel 3 XL",
        DeviceKind::Phone,
    );
    let key_hex = mac
        .engine
        .devices()
        .first()
        .expect("paired")
        .key_hex
        .clone();
    assert_eq!(mac.engine.devices()[0].mount_path, None);

    mac.engine
        .set_mount_path(key_hex.clone(), Some("/Volumes/Pixel 3 XL".to_owned()))
        .expect("a paired device accepts a mount path");
    assert_eq!(
        mac.engine.devices()[0].mount_path,
        Some("/Volumes/Pixel 3 XL".to_owned())
    );

    mac.engine
        .set_mount_path(key_hex, None)
        .expect("clearing the path succeeds");
    assert_eq!(mac.engine.devices()[0].mount_path, None);

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

    let endpoint = mac
        .engine
        .mount_start(phone_key_hex)
        .expect("mount_start");
    let addr: SocketAddr = format!("127.0.0.1:{}", port_of(&endpoint.url))
        .parse()
        .expect("a loopback address");

    // 32 plain connections, sending nothing, hold every slot:
    // `accept_loop` is one thread accepting strictly in order, so all 32
    // are fully reserved before it ever looks at a 33rd.
    let mut idle = Vec::with_capacity(MAX_LIVE_CONNECTIONS);
    for _ in 0..MAX_LIVE_CONNECTIONS {
        idle.push(TcpStream::connect(addr).expect("the bridge should accept up to its cap"));
    }
    // A generous margin for the accept loop's own thread to actually
    // reserve each slot and dispatch its connection thread; ordering
    // alone already guarantees it happens before the 33rd is looked at.
    std::thread::sleep(Duration::from_millis(200));

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
