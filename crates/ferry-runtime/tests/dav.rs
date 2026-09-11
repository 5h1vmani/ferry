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
    data: tempfile::TempDir,
    shared: tempfile::TempDir,
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
    build_side_with_roots(
        name,
        kind,
        own_key,
        &[RootSpec {
            name: "Root",
            writable: true,
            files,
        }],
        peer_key,
        peer_name,
        peer_kind,
    )
}

/// One shared root [`build_side_with_roots`] should create, and the files
/// to seed it with.
struct RootSpec<'a> {
    name: &'a str,
    writable: bool,
    files: &'a [(&'a str, &'a [u8])],
}

/// As [`build_side`], but for more than one shared root, or a root that
/// is not writable: `docs/engine-contract.md`, item 6, I2, needs both for
/// a cross-root `MOVE` and for a `PUT` the peer refuses.
#[allow(clippy::too_many_arguments)]
fn build_side_with_roots(
    name: &str,
    kind: DeviceKind,
    own_key: KeyPair,
    roots: &[RootSpec<'_>],
    peer_key: &KeyPair,
    peer_name: &str,
    peer_kind: DeviceKind,
) -> Side {
    let data = tempfile::tempdir().expect("a temporary folder for engine files");
    let shared = tempfile::tempdir().expect("a temporary folder for shared files");
    let download = tempfile::tempdir().expect("a temporary folder for downloaded files");
    let mut engine_roots = Vec::with_capacity(roots.len());
    for root in roots {
        let root_dir = shared.path().join(root.name);
        std::fs::create_dir_all(&root_dir).expect("a folder for a root");
        for (relative, contents) in root.files {
            let path = root_dir.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("a folder for a fixture file");
            }
            std::fs::write(path, contents).expect("a fixture file should write");
        }
        engine_roots.push(Root {
            name: root.name.to_owned(),
            path: root_dir.to_string_lossy().into_owned(),
            writable: root.writable,
        });
    }
    seed_peer(data.path(), peer_key, peer_name, peer_kind);

    let config = Config {
        data_dir: data.path().to_string_lossy().into_owned(),
        shared_roots: engine_roots,
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
        data,
        shared,
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

/// Counts every access log entry matching `actor` and `verb`.
fn count_entries(
    log: &[ferry_runtime::AccessEntry],
    actor: ferry_runtime::Actor,
    verb: ferry_runtime::AccessVerb,
) -> usize {
    log.iter()
        .filter(|entry| entry.actor == actor && entry.verb == verb)
        .count()
}

/// Sums `bytes` over every access log entry matching `actor` and `verb`.
fn sum_bytes(
    log: &[ferry_runtime::AccessEntry],
    actor: ferry_runtime::Actor,
    verb: ferry_runtime::AccessVerb,
) -> u64 {
    log.iter()
        .filter(|entry| entry.actor == actor && entry.verb == verb)
        .filter_map(|entry| entry.bytes)
        .sum()
}

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

#[test]
// I2-3: the spool file is never left behind, and its folder has a bound.
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

    // --- The spool folder's total size cap answers 507. A sparse file
    // reports the size `new_spool_path` checks against without this test
    // writing anywhere near 64 GiB of real bytes.
    std::fs::create_dir_all(&spool_dir).expect("the spool folder should exist");
    let sparse = spool_dir.join("already-huge");
    let file = std::fs::File::create(&sparse).expect("a sparse file should create");
    file.set_len(64 * 1024 * 1024 * 1024)
        .expect("a sparse file should grow without writing real bytes");
    drop(file);
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
    std::fs::remove_file(&sparse).expect("the sparse fixture should clean up");

    phone.engine.stop();
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

    let endpoint = mac.engine.mount_start(phone_key_hex).expect("mount_start");
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
