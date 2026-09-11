//! The accept loop, the per-connection loop, and the verbs.
//!
//! `docs/engine-contract.md`, item 6. I1: `OPTIONS`; `PROPFIND` at depth 0
//! and 1; `GET` and `HEAD`, with one `Range`; `LOCK` and `UNLOCK`; and a
//! `PUT` of a sidecar name. I2 begins here with `PUT` of a real file: a
//! spool file staged on this Mac, then landed on the peer with the push
//! rule (item 5) for a new destination, or with only the chunks that
//! differ, in place, for one that already exists.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use ferry_core::chunk::Manifest;
use ferry_core::localfs::LocalFs;
use ferry_core::noise::SecureStream;
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, RpcError};

use crate::access::AccessVerb;
use crate::engine::{Shared, record_this};
use crate::guard::StopAware;

use super::cache::Cache;
use super::lock::{LockError, LockTable};
use super::pool::{self, Pool};
use super::probes::{self, SidecarStore, SidecarWriteError};
use super::put;
use super::{http, xml};

/// How many connections one bridge serves at once. A 33rd is refused at
/// accept, before its socket is even read from. Mirrors
/// `MAX_PENDING_HANDSHAKES` in `ferry_core::tcp`.
const MAX_LIVE_CONNECTIONS: u32 = 32;

/// How long a connection may sit with nothing read, or a write may block,
/// before this bridge gives up on it. `docs/engine-contract.md`, item 6,
/// sets no number of its own; this exists only so a connection that never
/// sends a byte, or a peer that stops reading, cannot hold a thread
/// forever.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Reserves one live-connection slot, and gives it back when dropped.
/// Mirrors `PendingSlot` in `ferry_core::tcp`: a slot can only be created
/// while one is free, and dropping it is the only way to free one again,
/// so the count can never be missed or double counted.
struct ConnectionSlot(Arc<AtomicU32>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Reserves one of [`MAX_LIVE_CONNECTIONS`] slots, or `None` when the
/// bridge is already at that many.
fn reserve_connection_slot(connections: &Arc<AtomicU32>) -> Option<ConnectionSlot> {
    connections
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_LIVE_CONNECTIONS).then_some(current + 1)
        })
        .ok()?;
    Some(ConnectionSlot(Arc::clone(connections)))
}

/// One device's bridge state, shared by every connection thread serving
/// it.
pub(crate) struct Bridge {
    device_key_hex: String,
    /// The peer's own stored name, read once at `mount_start`. Used only
    /// as the mount root's `displayname` (N4); never anything a DAV
    /// request could shape.
    device_name: String,
    user: String,
    password: String,
    port: u16,
    sidecars: SidecarStore,
    pool: Pool,
    cache: Cache,
    locks: LockTable,
    /// Live connections right now, checked at accept against
    /// [`MAX_LIVE_CONNECTIONS`] (B3).
    connections: Arc<AtomicU32>,
}

impl Bridge {
    pub(crate) fn new(
        device_key_hex: String,
        device_name: String,
        user: String,
        password: String,
        port: u16,
        sidecar_dir: PathBuf,
    ) -> Self {
        Self {
            pool: Pool::new(device_key_hex.clone()),
            sidecars: SidecarStore::new(sidecar_dir),
            cache: Cache::new(),
            locks: LockTable::new(),
            connections: Arc::new(AtomicU32::new(0)),
            device_key_hex,
            device_name,
            user,
            password,
            port,
        }
    }
}

/// Accepts connections until `running` goes false, handing each to its own
/// thread. One accept thread, one thread per connection, per
/// `docs/engine-contract.md`, item 6, matching every other transport in
/// this engine.
pub(crate) fn accept_loop(
    shared: &Arc<Shared>,
    bridge: &Arc<Bridge>,
    listener: &TcpListener,
    running: &Arc<AtomicBool>,
) {
    while running.load(Ordering::SeqCst) {
        let Ok((stream, _addr)) = listener.accept() else {
            continue;
        };
        if !running.load(Ordering::SeqCst) {
            // This is `MountRegistry::stop`'s own wake-up connection, or a
            // real client that arrived in the instant stop began. Either
            // way nothing should be served on it.
            drop(stream);
            continue;
        }
        // B3: a stranger on loopback needs no password to reach this far,
        // so the live-connection cap is checked, and the timeouts are set,
        // before this connection's first byte is ever read.
        let Some(slot) = reserve_connection_slot(&bridge.connections) else {
            drop(stream);
            continue;
        };
        if stream.set_read_timeout(Some(CONNECTION_TIMEOUT)).is_err()
            || stream.set_write_timeout(Some(CONNECTION_TIMEOUT)).is_err()
        {
            drop(stream);
            continue;
        }
        let shared = Arc::clone(shared);
        let bridge = Arc::clone(bridge);
        // A connection thread is not joined, the same known limitation
        // `crate::lib` documents for every other transport here: it ends
        // when its peer (Finder, or a test client) closes its side.
        //
        // `Builder::spawn` rather than the panicking `thread::spawn`: when
        // the OS refuses a new thread, `stream` (moved into the closure
        // below) is simply dropped along with it, closing the socket, and
        // the accept loop keeps running instead of taking the whole
        // bridge down with it.
        let spawned = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            handle_connection(&shared, &bridge, &stream);
        });
        drop(spawned);
    }
}

/// Serves requests on one connection until it closes, a write fails, or a
/// request breaks a bound `respond` cannot recover from.
fn handle_connection(shared: &Arc<Shared>, bridge: &Arc<Bridge>, stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    loop {
        let Ok(outcome) = http::read_head(&mut reader) else {
            return;
        };
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        let head = match outcome {
            http::HeadOutcome::Head(head) => head,
            http::HeadOutcome::Closed => return,
            // B2: the head ran past its budget, or carried too many
            // headers. There is no safe place left to resume parsing the
            // next request from, so this answers once and closes.
            http::HeadOutcome::HeadTooLarge => {
                let _ = no_body(&mut out, "431 Request Header Fields Too Large");
                return;
            }
        };
        match respond(shared, bridge, &head, &mut reader, &mut out) {
            Ok(true) => {}
            Ok(false) | Err(_) => return,
        }
        if out.flush().is_err() {
            return;
        }
    }
}

/// Answers one request: `Host` and Basic auth first, from the head alone,
/// then the body and the verb.
///
/// Returns whether this connection may serve another request. `Host` and
/// auth failures close it: `docs/engine-contract.md`, item 6, I2, gives a
/// `PUT` of a real file a body far larger than [`http::MAX_BODY_LEN`], so
/// there is no bound this function could drain up to before answering
/// without paying for whatever a stranger on loopback claims to be
/// sending. Every other refusal keeps the connection open, once its own
/// declared body (bounded to [`http::MAX_BODY_LEN`] the same way as I1)
/// has actually been read.
fn respond(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    head: &http::RequestHead,
    reader: &mut impl BufRead,
    out: &mut impl Write,
) -> io::Result<bool> {
    let expected_host = format!("127.0.0.1:{}", bridge.port);
    if head.header("host") != Some(expected_host.as_str()) {
        no_body(out, "400 Bad Request")?;
        return Ok(false);
    }
    if !authorized(bridge, head.header("authorization")) {
        http::write_head(
            out,
            "401 Unauthorized",
            &[
                ("WWW-Authenticate", "Basic realm=\"Ferry\"".to_owned()),
                ("Content-Length", "0".to_owned()),
            ],
        )?;
        return Ok(false);
    }

    let decoded = http::percent_decode(&head.target);
    let target = decoded.trim_start_matches('/').trim_end_matches('/');
    let is_probe = probes::is_probe_name(probes::last_segment(target));

    // `docs/engine-contract.md`, item 6, I2: a `PUT` of a real file is
    // streamed straight to its spool file, never buffered whole, so it
    // is handled before the body is read the way every other route's is.
    if head.method == "PUT" && !is_probe {
        return put_file(shared, bridge, target, head, reader, out);
    }

    let content_length = head.content_length().unwrap_or(0);
    let body = match http::read_bounded_body(reader, content_length, http::MAX_BODY_LEN)? {
        // B1: unchanged from I1: refused before a single byte of the
        // (never sent, in a real client) body is read.
        http::BodyOutcome::TooLarge => {
            no_body(out, "413 Payload Too Large")?;
            return Ok(false);
        }
        http::BodyOutcome::Body(body) => body,
    };
    let request = http::Request {
        method: head.method.clone(),
        headers: head.headers.clone(),
        body,
    };

    if request.method == "OPTIONS" {
        return options(out).map(|()| true);
    }

    // `docs/engine-contract.md`, item 6, I2: a `.ferry-part` name is 404
    // on `GET` and `HEAD`, checked before the peer or the sidecar store,
    // since Finder must never see a file still landing.
    let is_partial = put::is_partial_name(probes::last_segment(target));

    match request.method.as_str() {
        "LOCK" => lock_verb(bridge, target, out),
        "UNLOCK" => unlock_verb(bridge, &request, target, out),
        "PUT" => put_sidecar(bridge, target, &request, out),
        "GET" | "HEAD" if is_partial => no_body(out, "404 Not Found"),
        "PROPFIND" if is_probe => propfind_probe(bridge, target, &request, out),
        "GET" | "HEAD" if is_probe => get_probe(bridge, target, &request.method, out),
        "PROPFIND" => propfind(shared, bridge, target, &request, out),
        "GET" | "HEAD" => get_file(shared, bridge, target, &request.method, &request, out),
        _ => method_not_allowed(out),
    }
    .map(|()| true)
}

fn no_body(out: &mut impl Write, status: &str) -> io::Result<()> {
    http::write_head(out, status, &[("Content-Length", "0".to_owned())])
}

/// N3: a 405 carries the methods this bridge answers, the same list
/// `options` states for `OPTIONS`.
fn method_not_allowed(out: &mut impl Write) -> io::Result<()> {
    http::write_head(
        out,
        "405 Method Not Allowed",
        &[
            ("Allow", ALLOWED_METHODS.to_owned()),
            ("Content-Length", "0".to_owned()),
        ],
    )
}

fn unavailable(out: &mut impl Write) -> io::Result<()> {
    no_body(out, "503 Service Unavailable")
}

/// Checks the `Authorization` header against `bridge`'s per-start
/// password. `bridge.user` is checked in the ordinary way; only the
/// password compare needs to be constant-time, since the user name is
/// fixed and not a secret.
///
/// Takes the header's value directly, rather than a whole request, so it
/// reads the same from the head alone (before any body is touched) as it
/// does once a request is fully buffered.
fn authorized(bridge: &Bridge, header: Option<&str>) -> bool {
    let Some(header) = header else {
        return false;
    };
    let Some(encoded) = header.strip_prefix("Basic ") else {
        return false;
    };
    let Some(decoded) = http::base64_decode(encoded) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((user, password)) = text.split_once(':') else {
        return false;
    };
    user == bridge.user && http::constant_time_eq(password.as_bytes(), bridge.password.as_bytes())
}

/// The methods this bridge answers at all, I1 and I2 together. Shared by
/// `OPTIONS` and by a 405's `Allow` header (N3).
const ALLOWED_METHODS: &str =
    "OPTIONS, GET, HEAD, PUT, PROPFIND, LOCK, UNLOCK";

fn options(out: &mut impl Write) -> io::Result<()> {
    http::write_head(
        out,
        "200 OK",
        &[
            ("Allow", ALLOWED_METHODS.to_owned()),
            ("MS-Author-Via", "DAV".to_owned()),
            ("Content-Length", "0".to_owned()),
        ],
    )
}

// ---------------------------------------------------------------------------
// LOCK and UNLOCK. Never reach the peer: `docs/spike-0-findings.md`,
// question 4.
// ---------------------------------------------------------------------------

fn lock_verb(bridge: &Bridge, target: &str, out: &mut impl Write) -> io::Result<()> {
    let token = match bridge.locks.lock_path(target) {
        Ok(token) => token,
        // S5: the table is full, its own storage rather than another
        // resource's lock in the way, so 507 rather than RFC 4918's 423.
        Err(LockError::Full) => return no_body(out, "507 Insufficient Storage"),
        Err(LockError::NoRandomness) => return no_body(out, "500 Internal Server Error"),
    };
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
<D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope>\
<D:depth>infinity</D:depth><D:timeout>Second-3600</D:timeout>\
<D:locktoken><D:href>{token}</D:href></D:locktoken>\
</D:activelock></D:lockdiscovery></D:prop>\n"
    );
    http::write_head(
        out,
        "200 OK",
        &[
            (
                "Content-Type",
                "application/xml; charset=\"utf-8\"".to_owned(),
            ),
            ("Lock-Token", format!("<{token}>")),
            ("Content-Length", body.len().to_string()),
        ],
    )?;
    out.write_all(body.as_bytes())
}

fn unlock_verb(
    bridge: &Bridge,
    request: &http::Request,
    target: &str,
    out: &mut impl Write,
) -> io::Result<()> {
    let token = request.header("lock-token").unwrap_or("");
    if bridge.locks.unlock_path(target, token) {
        no_body(out, "204 No Content")
    } else {
        no_body(out, "409 Conflict")
    }
}

// ---------------------------------------------------------------------------
// Probes: answered from the sidecar store, never from the peer.
// ---------------------------------------------------------------------------

fn propfind_probe(
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    let Some(sidecar) = bridge.sidecars.read(target) else {
        return no_body(out, "404 Not Found");
    };
    let props = xml::requested_props(&request.body);
    let name = probes::last_segment(target);
    let size = u64::try_from(sidecar.bytes.len()).unwrap_or(u64::MAX);
    let items = [xml::Item {
        path: target,
        name,
        is_dir: false,
        size,
        modified_unix_secs: sidecar.modified_unix_secs,
    }];
    write_multistatus(out, &items, &props)
}

fn get_probe(bridge: &Bridge, target: &str, method: &str, out: &mut impl Write) -> io::Result<()> {
    let Some(sidecar) = bridge.sidecars.read(target) else {
        return no_body(out, "404 Not Found");
    };
    let size = u64::try_from(sidecar.bytes.len()).unwrap_or(u64::MAX);
    http::write_head(
        out,
        "200 OK",
        &[
            ("Content-Length", sidecar.bytes.len().to_string()),
            ("Content-Type", "application/octet-stream".to_owned()),
            ("ETag", http::etag(size, sidecar.modified_unix_secs)),
            ("Last-Modified", http::rfc1123(sidecar.modified_unix_secs)),
        ],
    )?;
    if method == "GET" {
        out.write_all(&sidecar.bytes)?;
    }
    Ok(())
}

fn put_sidecar(
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    match bridge.sidecars.write(target, &request.body) {
        Ok(()) => {}
        // S4: a body over the sidecar bound is 413; a store already at
        // its file cap is 507, the same code the lock table's own cap
        // answers with (S5), for the same reason: this side's storage,
        // not another resource's lock.
        Err(SidecarWriteError::TooLarge) => return no_body(out, "413 Payload Too Large"),
        Err(SidecarWriteError::Full) => return no_body(out, "507 Insufficient Storage"),
        Err(SidecarWriteError::Io) => return no_body(out, "500 Internal Server Error"),
    }
    // A write through the bridge drops that folder's cached listing, per
    // `docs/engine-contract.md`, item 6, even though a sidecar is never
    // shown in one: Finder lists a folder again right after writing into
    // it, and the cache must not answer from before the write.
    bridge.cache.invalidate(parent_of(target));
    http::write_head(out, "201 Created", &[("Content-Length", "0".to_owned())])
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

// ---------------------------------------------------------------------------
// I2: the write verbs. `docs/engine-contract.md`, item 6, "I2, saving".
// ---------------------------------------------------------------------------

/// What to answer when a write verb's RPC call to the peer fails, and
/// whether the connection that produced it should be dropped rather than
/// reused.
///
/// Unlike `map_rpc_error` (I1, read only), `OpError::PermissionDenied`
/// here is exactly what it says: the receiver's writable flag on the root
/// (item 5), not a stand-in for the peer going away. I1 never calls an
/// operation the peer refuses for that reason; every I2 write verb can.
fn map_write_error(error: &RpcError) -> (&'static str, bool) {
    match error {
        RpcError::Remote(OpError::NotFound) => ("404 Not Found", false),
        RpcError::Remote(OpError::PermissionDenied) => ("403 Forbidden", false),
        RpcError::Remote(OpError::AlreadyExists) => ("405 Method Not Allowed", false),
        RpcError::Remote(OpError::NotADirectory | OpError::IsADirectory | OpError::NotEmpty) => {
            ("409 Conflict", false)
        }
        // `docs/engine-contract.md`, item 6: "across roots it is 502",
        // since `Roots::rename`'s own refusal (`Unsupported`) is the
        // peer's structure, not this bridge's, to answer for.
        RpcError::Remote(OpError::Unsupported) => ("502 Bad Gateway", false),
        RpcError::Remote(OpError::RangeTooLarge | OpError::Internal) => {
            ("500 Internal Server Error", false)
        }
        // Anything that is not an answer from the peer is the connection
        // itself failing, the same reasoning `map_rpc_error` carries for
        // I1: the peer stopped (item 16c), the cable went, or the stream
        // broke.
        _ => ("503 Service Unavailable", true),
    }
}

/// `PUT` of a real file. `docs/engine-contract.md`, item 6, I2: the body
/// is spooled to disk in pieces as it arrives, never held whole in
/// memory. A new destination lands the push way
/// ([`put::land_new`]); an existing one lands the chunks that differ, in
/// place ([`put::land_delta`]), which is the delta on save.
fn put_file(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    target: &str,
    head: &http::RequestHead,
    reader: &mut impl BufRead,
    out: &mut impl Write,
) -> io::Result<bool> {
    let Some(content_length) = head.content_length() else {
        // No declared length at all: there is nothing safe to drain
        // before the next request, so this closes rather than guessing,
        // the same reasoning `BodyTooLarge` already carries in `respond`.
        no_body(out, "411 Length Required")?;
        return Ok(false);
    };
    if content_length > http::MAX_PUT_BODY_LEN {
        // A declared length is known here, but paying to drain up to 32
        // GiB just to keep a connection alive is not worth it either;
        // Finder reconnects, the same as after a 413 anywhere else in
        // this bridge.
        no_body(out, "413 Payload Too Large")?;
        return Ok(false);
    }

    // From here on, `content_length` is known and within bounds, so a
    // refusal drains the declared body (in bounded pieces, to
    // `io::sink()`, never held in memory) and keeps the connection open,
    // the way Finder expects across an ordinary `LOCK`-`PUT`-`UNLOCK`
    // sequence on one connection.
    macro_rules! refuse {
        ($status:expr) => {{
            http::copy_body(reader, content_length, &mut io::sink())?;
            no_body(out, $status)?;
            return Ok(true);
        }};
    }

    let Ok(path) = RemotePath::parse(target) else {
        refuse!("404 Not Found");
    };
    if path.is_root() {
        refuse!("404 Not Found");
    }
    let Some(spool_path) = put::new_spool_path(shared, &bridge.device_key_hex) else {
        refuse!("500 Internal Server Error");
    };

    // A short body or an I/O failure here propagates as an `Err`, closing
    // the connection: the same reasoning `stream_body`'s own S2 carries
    // for a `GET`, in the write direction. Everything from here on
    // answers a real response instead, since the body has, by this
    // point, been received in full and correctly.
    put::spool_body(reader, content_length, &spool_path)?;

    let Some((fs, leaf, manifest)) = put::open_spool(&spool_path) else {
        let _ = std::fs::remove_file(&spool_path);
        return no_body(out, "500 Internal Server Error").map(|()| true);
    };
    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        let _ = std::fs::remove_file(&spool_path);
        return unavailable(out).map(|()| true);
    };
    let landing = put_landing(&mut borrowed, &path, &fs, &leaf, &manifest);
    let _ = std::fs::remove_file(&spool_path);

    match landing {
        Ok(kind) => {
            bridge.cache.invalidate(parent_of(target));
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Write,
                path.as_str(),
                Some(manifest.length()),
                None,
                None,
            );
            let status = match kind {
                Landing::New => "201 Created",
                Landing::Delta => "204 No Content",
            };
            no_body(out, status).map(|()| true)
        }
        // A `PUT` onto an existing folder's path fits no verb this
        // bridge otherwise answers with 409, so it is named here rather
        // than folded into `map_write_error`'s generic mapping.
        Err(RpcError::Remote(OpError::IsADirectory)) => no_body(out, "409 Conflict").map(|()| true),
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            no_body(out, status).map(|()| true)
        }
    }
}

/// Which of `put.rs`'s two landing rules [`put_landing`] used, so
/// `put_file` answers 201 or 204.
enum Landing {
    New,
    Delta,
}

/// Stats `destination` to decide which landing rule applies, then runs
/// it. One function so `put_file` never holds two overlapping mutable
/// borrows of `borrowed`'s connection at once.
fn put_landing(
    borrowed: &mut pool::Borrowed<'_>,
    destination: &RemotePath,
    fs: &LocalFs,
    leaf: &RemotePath,
    manifest: &Manifest,
) -> Result<Landing, RpcError> {
    match borrowed.client().stat(destination) {
        Ok(entry) if entry.kind == FileKind::Directory => {
            Err(RpcError::Remote(OpError::IsADirectory))
        }
        Ok(_) => {
            put::land_delta(borrowed.client(), destination, fs, leaf, manifest)?;
            Ok(Landing::Delta)
        }
        Err(RpcError::Remote(OpError::NotFound)) => {
            put::land_new(borrowed.client(), destination, fs, leaf, manifest)?;
            Ok(Landing::New)
        }
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Real paths: served through the pool.
// ---------------------------------------------------------------------------

/// N3, RFC 4918 9.1: a `PROPFIND` this bridge will not walk. `depth` is
/// `None` for a missing header and `Some("infinity")` for an explicit one;
/// both mean "the whole tree", which I1 never serves.
fn depth_not_finite(out: &mut impl Write) -> io::Result<()> {
    let body = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<D:error xmlns:D=\"DAV:\"><D:propfind-finite-depth/></D:error>\n";
    http::write_head(
        out,
        "403 Forbidden",
        &[
            (
                "Content-Type",
                "application/xml; charset=\"utf-8\"".to_owned(),
            ),
            ("Content-Length", body.len().to_string()),
        ],
    )?;
    out.write_all(body.as_bytes())
}

fn propfind(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    let Ok(path) = RemotePath::parse(target) else {
        return no_body(out, "404 Not Found");
    };
    let Some(depth) = request.header("depth") else {
        return depth_not_finite(out);
    };
    if depth == "infinity" {
        return depth_not_finite(out);
    }

    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out);
    };

    let self_entry = match borrowed.client().stat(&path) {
        Ok(entry) => entry,
        Err(error) => {
            let (status, unhealthy) = map_rpc_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    };

    let mut named: Vec<(String, Entry)> = vec![(target.to_owned(), self_entry.clone())];
    let mut listed = false;

    if depth != "0" && self_entry.kind == FileKind::Directory {
        listed = true;
        let cached = bridge.cache.get(target);
        let children = match cached {
            Some(children) => children,
            None => match list_all(borrowed.client(), &path) {
                Ok(children) => {
                    bridge.cache.put(target, children.clone());
                    children
                }
                Err(error) => {
                    let (status, unhealthy) = map_rpc_error(&error);
                    if unhealthy {
                        borrowed.mark_unhealthy();
                    }
                    return no_body(out, status);
                }
            },
        };
        for child in children {
            // A real file that happens to share a probe name is still
            // never shown: those names belong to the sidecar store in
            // this bridge's model. `docs/engine-contract.md`, item 6.
            // Neither is a landing file's own `.ferry-part` name, item 6,
            // I2: Finder must never see one.
            if probes::is_probe_name(&child.name) || put::is_partial_name(&child.name) {
                continue;
            }
            let child_path = if target.is_empty() {
                child.name.clone()
            } else {
                format!("{target}/{}", child.name)
            };
            named.push((child_path, child));
        }
    }
    drop(borrowed);

    // S3: a bridge `PROPFIND` leaves a `This` entry in the Mac's own
    // access log, once the peer round trip it needed is done. A folder
    // listing is `List` with the entries actually shown (children only,
    // matching `Engine::list`'s own count); a single stat, at depth 0 or
    // on a file, is `Stat`.
    if listed {
        let entry_count = u32::try_from(named.len().saturating_sub(1)).unwrap_or(u32::MAX);
        record_this(
            shared,
            &bridge.device_key_hex,
            AccessVerb::List,
            path.as_str(),
            None,
            Some(entry_count),
            None,
        );
    } else {
        record_this(
            shared,
            &bridge.device_key_hex,
            AccessVerb::Stat,
            path.as_str(),
            None,
            None,
            None,
        );
    }

    let props = xml::requested_props(&request.body);
    let items: Vec<xml::Item<'_>> = named
        .iter()
        .map(|(path, entry)| xml::Item {
            path,
            name: if path.is_empty() {
                // The mount root has no last path segment of its own;
                // N4 names it after the device instead.
                bridge.device_name.as_str()
            } else {
                probes::last_segment(path)
            },
            is_dir: entry.kind == FileKind::Directory,
            size: entry.size,
            modified_unix_secs: entry.modified_unix_secs,
        })
        .collect();
    write_multistatus(out, &items, &props)
}

fn write_multistatus(
    out: &mut impl Write,
    items: &[xml::Item<'_>],
    props: &xml::PropSet,
) -> io::Result<()> {
    let body = xml::multistatus(items, props);
    http::write_head(
        out,
        "207 Multi-Status",
        &[
            (
                "Content-Type",
                "application/xml; charset=\"utf-8\"".to_owned(),
            ),
            ("Content-Length", body.len().to_string()),
        ],
    )?;
    out.write_all(body.as_bytes())
}

/// Pages through the peer's `list` cursor, the same bound `Engine::list`
/// uses. A folder past the bound is served truncated rather than failing
/// the whole `PROPFIND`: `docs/engine-contract.md`, item 6, sets no folder
/// size limit for browsing, so a partial listing is the more useful answer
/// than none.
fn list_all(
    client: &mut Client<StopAware<SecureStream>>,
    path: &RemotePath,
) -> Result<Vec<Entry>, RpcError> {
    let mut entries = Vec::new();
    let mut cursor = 0u64;
    let mut pages = 0usize;
    let mut seen = 0usize;
    loop {
        let (page, next_cursor) = client.list(path, cursor)?;
        pages += 1;
        seen += page.len();
        entries.extend(page);
        match crate::folder::after_page(cursor, next_cursor, pages, seen) {
            Ok(Some(next)) => cursor = next,
            Ok(None) | Err(_) => break,
        }
    }
    Ok(entries)
}

fn get_file(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    target: &str,
    method: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    let Ok(path) = RemotePath::parse(target) else {
        return no_body(out, "404 Not Found");
    };
    if path.is_root() {
        return no_body(out, "404 Not Found");
    }

    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out);
    };

    let entry = match borrowed.client().stat(&path) {
        Ok(entry) => entry,
        Err(error) => {
            let (status, unhealthy) = map_rpc_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    };
    if entry.kind == FileKind::Directory {
        return no_body(out, "404 Not Found");
    }

    // S1: a `Range` this side cannot satisfy is 416, with `Content-Range:
    // bytes */<size>`, never a 200 with the wrong length.
    let outcome = request
        .header("range")
        .map_or(http::RangeOutcome::Absent, |value| {
            http::parse_range(value, entry.size)
        });
    let (start, end, ranged) = match outcome {
        http::RangeOutcome::Unsatisfiable => {
            return http::write_head(
                out,
                "416 Range Not Satisfiable",
                &[
                    ("Content-Range", format!("bytes */{}", entry.size)),
                    ("Content-Length", "0".to_owned()),
                ],
            );
        }
        http::RangeOutcome::Absent => (0, entry.size.saturating_sub(1), false),
        http::RangeOutcome::Satisfiable(start, end) => (start, end, true),
    };
    let len = if entry.size == 0 {
        0
    } else {
        end.saturating_sub(start) + 1
    };

    let status = if ranged {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut headers = vec![
        ("Content-Length", len.to_string()),
        ("Content-Type", "application/octet-stream".to_owned()),
        ("Accept-Ranges", "bytes".to_owned()),
        ("ETag", http::etag(entry.size, entry.modified_unix_secs)),
        ("Last-Modified", http::rfc1123(entry.modified_unix_secs)),
    ];
    if ranged {
        headers.push((
            "Content-Range",
            format!("bytes {start}-{end}/{}", entry.size),
        ));
    }
    http::write_head(out, status, &headers)?;

    let sent = if method == "HEAD" || len == 0 {
        0
    } else {
        // S2: a peer read failing mid body propagates as an `Err`, so
        // `handle_connection` drops the connection instead of trying to
        // parse a next request off a socket whose promised
        // `Content-Length` was never met.
        stream_body(&mut borrowed, &path, start, end.saturating_add(1), out)?
    };
    // S3: a bridge `GET` leaves a `This` entry in the Mac's own access
    // log, once the peer round trip is done.
    record_this(
        shared,
        &bridge.device_key_hex,
        AccessVerb::Read,
        path.as_str(),
        Some(sent),
        None,
        None,
    );
    Ok(())
}

/// Streams `[start, stop_at)` of `path` from the peer to `out`, in pieces
/// of at most one mebibyte, whether or not a `Range` was asked for, per
/// `docs/engine-contract.md`, item 6. Stops at the first failed write to
/// the socket: `docs/spike-0-findings.md`, question 2, found macOS aborts
/// an open-ended range early, and a naive server that tried to hand it a
/// whole 512 MiB file at once wasted the read.
///
/// # Errors
///
/// Returns an error when the peer's `read` fails (S2: the caller's
/// `Content-Length` promise can no longer be met, so the connection must
/// be dropped, not reused for a next request) or when the write to `out`
/// fails.
fn stream_body(
    borrowed: &mut pool::Borrowed<'_>,
    path: &RemotePath,
    start: u64,
    stop_at: u64,
    out: &mut impl Write,
) -> io::Result<u64> {
    let piece = ferry_core::limits::MAX_READ_LEN;
    let mut offset = start;
    let mut sent = 0u64;
    while offset < stop_at {
        let want = u32::try_from((stop_at - offset).min(u64::from(piece))).unwrap_or(piece);
        let bytes = match borrowed.client().read(path, offset, want) {
            Ok(bytes) => bytes,
            Err(error) => {
                let (_, unhealthy) = map_rpc_error(&error);
                if unhealthy {
                    borrowed.mark_unhealthy();
                }
                return Err(io::Error::other(
                    "the peer's read failed before the declared Content-Length was met",
                ));
            }
        };
        if bytes.is_empty() {
            break;
        }
        out.write_all(&bytes)?;
        let written = u64::try_from(bytes.len()).unwrap_or(0);
        sent += written;
        offset += written;
    }
    Ok(sent)
}

/// What to answer for one failed operation on the peer, and whether the
/// connection that produced it should be dropped rather than reused.
fn map_rpc_error(error: &RpcError) -> (&'static str, bool) {
    match error {
        RpcError::Remote(OpError::NotFound) => ("404 Not Found", false),
        // I1 only ever calls `stat`, `list`, and `read`, none of which the
        // peer's `Roots` refuses for a real reason (`roots.rs`): a
        // `PermissionDenied` answer here can only mean the peer's own
        // `stop` or `forget` switched this connection off. Reported as the
        // device going away, which is what it is from Finder's side.
        RpcError::Remote(OpError::PermissionDenied) => ("503 Service Unavailable", true),
        RpcError::Remote(_) => ("500 Internal Server Error", false),
        // Anything that is not an answer from the peer is the connection
        // itself failing: the peer stopped and closed the socket
        // (`docs/engine-contract.md` item 16c), the cable went, or the
        // stream broke. From Finder's side the device went away.
        _ => ("503 Service Unavailable", true),
    }
}
