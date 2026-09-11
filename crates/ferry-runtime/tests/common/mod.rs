//! What the `dav_*.rs` files and `tests/item_17_prefetch.rs` all need to
//! talk to a running bridge: two paired engines in one process, and a
//! hand-written HTTP client over `TcpStream`.
//!
//! `docs/engine-contract.md`, item 17, asks for this file so the two test
//! files share one client instead of copying it. Every item here moved
//! from the bridge tests unchanged apart from its visibility.
//!
//! `engines.rs` beside this file holds the two-engine harness that five
//! test files used to copy: the listener, the inbox, one `Side` per
//! engine, and the code-method pairing helper.
//!
//! Each test binary that says `mod common;` compiles this whole file, so a
//! binary that uses only part of it would otherwise warn about the rest.

#![allow(dead_code)]

pub(crate) mod engines;
pub(crate) mod paths;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use ferry_core::noise::PublicKey;
use ferry_core::peers::{DeviceKind as CoreDeviceKind, Peer, PeerStore};
use ferry_runtime::{Config, DeviceKind, Engine, EngineListener, KeyPair, PairingState, Root};

/// How long any wait may take before the test gives up.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

/// A listener that does nothing. This file dials real sockets and reads
/// real answers; it never needs a change notification.
pub(crate) struct Silent;

impl EngineListener for Silent {
    fn devices_changed(&self) {}
    fn transfers_changed(&self) {}
    fn pairing_changed(&self, _state: PairingState) {}
    fn access_log_changed(&self) {}
}

/// One engine and the folders it owns, kept alive for the life of the
/// test.
pub(crate) struct Side {
    pub(crate) engine: std::sync::Arc<Engine>,
    pub(crate) data: tempfile::TempDir,
    pub(crate) shared: tempfile::TempDir,
    pub(crate) _download: tempfile::TempDir,
}

pub(crate) fn public_key_of(key: &KeyPair) -> PublicKey {
    let mut out = [0u8; 32];
    out.copy_from_slice(&key.public);
    PublicKey(out)
}

pub(crate) fn core_kind(kind: DeviceKind) -> CoreDeviceKind {
    match kind {
        DeviceKind::Phone => CoreDeviceKind::Phone,
        DeviceKind::Mac => CoreDeviceKind::Mac,
    }
}

/// Writes a peer store that already holds the other side, so the two
/// engines recognise each other without running the pairing handshake:
/// this file tests the bridge, not pairing, which `two_engines_pairing.rs` already
/// covers.
pub(crate) fn seed_peer(
    data_dir: &std::path::Path,
    peer_key: &KeyPair,
    peer_name: &str,
    peer_kind: DeviceKind,
) {
    let mut peers = PeerStore::load(&data_dir.join("peers.bin"), core_kind(peer_kind))
        .expect("a missing peer store loads empty");
    peers
        .add(Peer {
            key: public_key_of(peer_key),
            name: peer_name.to_owned(),
            paired_unix_secs: 0,
            kind: core_kind(peer_kind),
        })
        .expect("a fresh store is well under MAX_PEERS");
    peers.save().expect("the seeded peer store should save");
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_side(
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
pub(crate) struct RootSpec<'a> {
    pub(crate) name: &'a str,
    pub(crate) writable: bool,
    pub(crate) files: &'a [(&'a str, &'a [u8])],
}

/// As [`build_side`], but for more than one shared root, or a root that
/// is not writable: `docs/engine-contract.md`, item 6, I2, needs both for
/// a cross-root `MOVE` and for a `PUT` the peer refuses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_side_with_roots(
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

pub(crate) fn loopback_addr(side: &Side) -> SocketAddr {
    let bound = side.engine.listen_addr().expect("a bound listener");
    SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        bound.port(),
    )
}

/// The port named in a `MountEndpoint.url`, which is always
/// `"http://127.0.0.1:<port>/"`.
pub(crate) fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix('/'))
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("unexpected mount url shape: {url}"))
}

/// Standard base64, the encoding half of what `dav::http::base64_decode`
/// reads. Written out for the same reason the bridge itself writes its own:
/// no base64 crate is a dependency of this workspace.
pub(crate) fn base64_encode(input: &[u8]) -> String {
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
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// A hand-written HTTP/1.1 client over one `TcpStream`, kept open across
/// requests the way Finder keeps its own connection open.
pub(crate) struct TestClient {
    pub(crate) write_half: TcpStream,
    pub(crate) reader: BufReader<TcpStream>,
}

impl TestClient {
    pub(crate) fn connect(addr: SocketAddr) -> Self {
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
    pub(crate) fn request(
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
    pub(crate) fn write_raw_head(&mut self, head: &str) {
        self.write_half
            .write_all(head.as_bytes())
            .expect("a hand-built request head should write");
    }

    pub(crate) fn read_response(&mut self, no_body: bool) -> Response {
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
pub(crate) fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect()
}

/// Counts every access log entry matching `actor` and `verb`.
pub(crate) fn count_entries(
    log: &[ferry_runtime::AccessEntry],
    actor: ferry_runtime::Actor,
    verb: ferry_runtime::AccessVerb,
) -> usize {
    log.iter()
        .filter(|entry| entry.actor == actor && entry.verb == verb)
        .count()
}

/// Sums `bytes` over every access log entry matching `actor` and `verb`.
pub(crate) fn sum_bytes(
    log: &[ferry_runtime::AccessEntry],
    actor: ferry_runtime::Actor,
    verb: ferry_runtime::AccessVerb,
) -> u64 {
    log.iter()
        .filter(|entry| entry.actor == actor && entry.verb == verb)
        .filter_map(|entry| entry.bytes)
        .sum()
}
