//! The accept loop, the per-connection loop, and the verbs.
//!
//! `docs/engine-contract.md`, item 6. I1: `OPTIONS`; `PROPFIND` at depth 0
//! and 1; `GET` and `HEAD`, with one `Range`; `LOCK` and `UNLOCK`; and a
//! `PUT` of a sidecar name. I2 adds `PUT` of a real file, `MKCOL`,
//! `DELETE`, `MOVE`, `COPY`, and `PROPPATCH`, and the `If` header's lock
//! check on all of them.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use ferry_core::chunk::Manifest;
use ferry_core::localfs::LocalFs;
use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::RpcError;

use crate::access::AccessVerb;
use crate::engine::{Shared, record_this};
use crate::folder::RemoteLister;

use crate::pool::{self, Pool};

use super::cache::Cache;
use super::delete;
use super::heads::{self, HeadCache, Prefetch};
use super::http;
use super::lock::{LockError, LockTable, UnlockOutcome};
use super::probes::{self, SidecarStore, SidecarWriteError};
use super::put;

use crate::dav::handlers::{propfind, propfind_probe, proppatch_verb};

use crate::dav::handlers::{get_file, get_probe};

/// How many connections one bridge serves at once. A 33rd is refused at
/// accept, before its socket is even read from. Mirrors
/// `MAX_PENDING_HANDSHAKES` in `ferry_core::tcp`.
pub(crate) const MAX_LIVE_CONNECTIONS: u32 = 32;

/// How long a connection may sit with nothing read, or a write may block,
/// before this bridge gives up on it. `docs/engine-contract.md`, item 6,
/// sets no number of its own; this exists only so a connection that never
/// sends a byte, or a peer that stops reading, cannot hold a thread
/// forever.
pub(crate) const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a connection may go before its first request head is fully
/// read, in seconds, before this bridge gives up on it.
///
/// `docs/audits/fable-security.md`, finding 7: a connection that sends
/// nothing used to hold a slot for the whole thirty second
/// [`CONNECTION_TIMEOUT`]. `handle_connection` uses this instead, for the
/// very first [`http::read_head`] call only; every request after that on
/// the same kept-alive connection goes back to [`CONNECTION_TIMEOUT`],
/// since Finder holding a connection open between requests on purpose is
/// not what this bounds.
pub(crate) const FIRST_HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest number of connections that may be open without yet having
/// sent one request this bridge accepted as authorized, at once.
///
/// `docs/audits/fable-security.md`, finding 7: reaching this far takes no
/// password, unlike a paired TCP connection, so a local process opening
/// connections and never authenticating on any of them would otherwise be
/// bounded only by [`MAX_LIVE_CONNECTIONS`], holding every slot Finder's
/// own, already-authenticated connections need. `accept_loop` refuses a
/// fifth such connection the same way it refuses a 33rd live one: at once,
/// before its socket is even read from. A connection stops counting
/// against this the moment `authorized` first accepts it, so an ordinary
/// Finder session past its first request never sits here at all.
pub(crate) const MAX_UNAUTHENTICATED_CONNECTIONS: u32 = 4;

/// Reserves one live-connection slot, and gives it back when dropped.
/// Mirrors `PendingSlot` in `ferry_core::tcp`: a slot can only be created
/// while one is free, and dropping it is the only way to free one again,
/// so the count can never be missed or double counted.
pub(crate) struct ConnectionSlot(Arc<AtomicU32>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Reserves one of [`MAX_LIVE_CONNECTIONS`] slots, or `None` when the
/// bridge is already at that many.
pub(crate) fn reserve_connection_slot(connections: &Arc<AtomicU32>) -> Option<ConnectionSlot> {
    connections
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_LIVE_CONNECTIONS).then_some(current + 1)
        })
        .ok()?;
    Some(ConnectionSlot(Arc::clone(connections)))
}

/// Reserves one of [`MAX_UNAUTHENTICATED_CONNECTIONS`] slots, or `None`
/// when the bridge already holds that many connections that have not yet
/// authenticated. Shares [`ConnectionSlot`] with
/// [`reserve_connection_slot`]: both only ever decrement the counter they
/// were built from, so the same guard works for either.
pub(crate) fn reserve_unauth_slot(unauthenticated: &Arc<AtomicU32>) -> Option<ConnectionSlot> {
    unauthenticated
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_UNAUTHENTICATED_CONNECTIONS).then_some(current + 1)
        })
        .ok()?;
    Some(ConnectionSlot(Arc::clone(unauthenticated)))
}

/// One device's bridge state, shared by every connection thread serving
/// it.
pub(crate) struct Bridge {
    pub(crate) device_key_hex: String,
    /// The peer's own stored name, read once at `mount_start`. Used only
    /// as the mount root's `displayname` (N4); never anything a DAV
    /// request could shape.
    pub(crate) device_name: String,
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) port: u16,
    pub(crate) sidecars: SidecarStore,
    /// This device's pool, owned by [`Shared`] and shared with
    /// [`crate::Engine::list`]. `docs/engine-contract.md`, item 19.
    pub(crate) pool: Arc<Pool>,
    pub(crate) cache: Cache,
    /// Item 17: the first bytes of each recently listed image.
    pub(crate) heads: HeadCache,
    /// Item 17: the listing the prefetch thread works on next.
    pub(crate) prefetch: Prefetch,
    pub(crate) locks: LockTable,
    /// Live connections right now, checked at accept against
    /// [`MAX_LIVE_CONNECTIONS`] (B3).
    pub(crate) connections: Arc<AtomicU32>,
    /// Connections right now that have not yet sent one request this
    /// bridge accepted as authorized, checked at accept against
    /// [`MAX_UNAUTHENTICATED_CONNECTIONS`]. `docs/audits/fable-security.md`,
    /// finding 7.
    pub(crate) unauthenticated: Arc<AtomicU32>,
}

impl Bridge {
    pub(crate) fn new(
        device_key_hex: String,
        device_name: String,
        user: String,
        password: String,
        port: u16,
        sidecar_dir: PathBuf,
        pool: Arc<Pool>,
    ) -> Self {
        Self {
            pool,
            sidecars: SidecarStore::new(sidecar_dir),
            cache: Cache::new(),
            heads: HeadCache::new(),
            prefetch: Prefetch::new(),
            locks: LockTable::new(),
            connections: Arc::new(AtomicU32::new(0)),
            unauthenticated: Arc::new(AtomicU32::new(0)),
            device_key_hex,
            device_name,
            user,
            password,
            port,
        }
    }

    /// Ends the prefetch queue, so this bridge's prefetch thread leaves
    /// instead of waiting for a listing. Called by `MountRegistry::stop`,
    /// which lives in the parent module and cannot reach the field itself.
    pub(crate) fn stop_prefetch(&self) {
        self.prefetch.stop();
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
        // `docs/audits/fable-security.md`, finding 7: checked the same way
        // and at the same point as the cap above, so an unauthenticated
        // connection costs this bridge nothing beyond the accept itself
        // once it is refused.
        let Some(unauth_slot) = reserve_unauth_slot(&bridge.unauthenticated) else {
            drop(stream);
            continue;
        };
        // Finding 7: the read timeout starts at `FIRST_HEAD_TIMEOUT`, five
        // seconds, rather than the full `CONNECTION_TIMEOUT`; the write
        // timeout is unaffected, since the finding is about a connection
        // that sends nothing, not one that stops reading.
        if stream.set_read_timeout(Some(FIRST_HEAD_TIMEOUT)).is_err()
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
            handle_connection(&shared, &bridge, &stream, unauth_slot);
        });
        drop(spawned);
    }
}

/// Serves requests on one connection until it closes, a write fails, or a
/// request breaks a bound `respond` cannot recover from.
///
/// `unauth_slot` is held until this connection's first request authorizes,
/// which is when it is dropped, freeing the slot for another connection to
/// use while this one keeps serving under its ordinary `connections` slot.
/// `docs/audits/fable-security.md`, finding 7.
pub(crate) fn handle_connection(
    shared: &Arc<Shared>,
    bridge: &Arc<Bridge>,
    stream: &TcpStream,
    unauth_slot: ConnectionSlot,
) {
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut unauth_slot = Some(unauth_slot);
    let mut first_head = true;
    loop {
        let Ok(outcome) = http::read_head(&mut reader) else {
            return;
        };
        if first_head {
            first_head = false;
            // The head, however many reads that took, arrived inside
            // `FIRST_HEAD_TIMEOUT`. A later request on this same kept-alive
            // connection, which Finder holds open between requests on
            // purpose, gets the ordinary `CONNECTION_TIMEOUT` instead.
            if stream.set_read_timeout(Some(CONNECTION_TIMEOUT)).is_err() {
                return;
            }
        }
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
        let mut authorized_now = false;
        let outcome = respond(
            shared,
            bridge,
            &head,
            &mut reader,
            &mut out,
            &mut authorized_now,
        );
        if authorized_now {
            // Drops the slot at once, whatever `outcome` turns out to be:
            // credentials that check out are what finding 7 asks this cap
            // to stop counting, even when this one request then fails for
            // some other reason.
            drop(unauth_slot.take());
        }
        match outcome {
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
/// Returns whether this connection may serve another request, and sets
/// `*authorized_now` to `true` the moment `authorized` accepts this
/// request's credentials, whatever this call goes on to return: finding 7
/// of `docs/audits/fable-security.md` cares only about whether this
/// connection has ever proven a real password, not about this one
/// request's own outcome.
///
/// `Host` and auth failures close it: `docs/engine-contract.md`, item 6,
/// I2, gives a `PUT` of a real file a body far larger than
/// [`http::MAX_BODY_LEN`], so there is no bound this function could drain
/// up to before answering without paying for whatever a stranger on
/// loopback claims to be sending. Every other refusal keeps the connection
/// open, once its own declared body (bounded to [`http::MAX_BODY_LEN`] the
/// same way as I1) has actually been read.
pub(crate) fn respond(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    head: &http::RequestHead,
    reader: &mut impl BufRead,
    out: &mut impl Write,
    authorized_now: &mut bool,
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
    *authorized_now = true;

    // This bridge never speaks chunked transfer encoding: its body reading,
    // on every verb, trusts `Content-Length` alone. A request carrying
    // `Transfer-Encoding` at all cannot be framed safely against that, so
    // it is refused the same way a `PUT` with no `Content-Length` is,
    // before a single body byte is read.
    if head.header("transfer-encoding").is_some() {
        no_body(out, "411 Length Required")?;
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
    // on `GET`, `HEAD`, and `PROPFIND`, checked before the peer or the
    // sidecar store, since Finder must never see a file still landing.
    let is_partial = put::is_partial_name(probes::last_segment(target));

    match request.method.as_str() {
        "LOCK" => lock_verb(bridge, target, out),
        "UNLOCK" => unlock_verb(bridge, &request, target, out),
        "PUT" => put_sidecar(bridge, target, &request, out),
        "GET" | "HEAD" | "PROPFIND" if is_partial => no_body(out, "404 Not Found"),
        "PROPFIND" if is_probe => propfind_probe(bridge, target, &request, out),
        "GET" | "HEAD" if is_probe => get_probe(bridge, target, &request.method, out),
        "PROPFIND" => propfind(shared, bridge, target, &request, out),
        "GET" | "HEAD" => get_file(shared, bridge, target, &request.method, &request, out),
        "MKCOL" => mkcol_verb(shared, bridge, target, &request, out),
        "DELETE" => delete_verb(shared, bridge, target, &request, out),
        "MOVE" => move_verb(shared, bridge, target, &request, out),
        "COPY" => copy_verb(shared, bridge, target, &request, out),
        "PROPPATCH" => proppatch_verb(shared, bridge, target, &request, out),
        _ => method_not_allowed(out),
    }
    .map(|()| true)
}

pub(crate) fn no_body(out: &mut impl Write, status: &str) -> io::Result<()> {
    http::write_head(out, status, &[("Content-Length", "0".to_owned())])
}

/// N3: a 405 carries the methods this bridge answers, the same list
/// `options` states for `OPTIONS`.
pub(crate) fn method_not_allowed(out: &mut impl Write) -> io::Result<()> {
    http::write_head(
        out,
        "405 Method Not Allowed",
        &[
            ("Allow", ALLOWED_METHODS.to_owned()),
            ("Content-Length", "0".to_owned()),
        ],
    )
}

pub(crate) fn unavailable(out: &mut impl Write) -> io::Result<()> {
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
pub(crate) fn authorized(bridge: &Bridge, header: Option<&str>) -> bool {
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
pub(crate) const ALLOWED_METHODS: &str =
    "OPTIONS, GET, HEAD, PUT, PROPFIND, PROPPATCH, MKCOL, DELETE, MOVE, COPY, LOCK, UNLOCK";

pub(crate) fn options(out: &mut impl Write) -> io::Result<()> {
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

pub(crate) fn lock_verb(bridge: &Bridge, target: &str, out: &mut impl Write) -> io::Result<()> {
    let token = match bridge.locks.lock_path(target) {
        Ok(token) => token,
        // A second `LOCK` of an unexpired lock, RFC 4918's own 423.
        Err(LockError::AlreadyLocked) => return no_body(out, "423 Locked"),
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

pub(crate) fn unlock_verb(
    bridge: &Bridge,
    request: &http::Request,
    target: &str,
    out: &mut impl Write,
) -> io::Result<()> {
    let token = request.header("lock-token").unwrap_or("");
    match bridge.locks.unlock_path(target, token) {
        UnlockOutcome::Unlocked => no_body(out, "204 No Content"),
        // A lock is held, but the token given does not name it: something
        // real is being refused, unlike a path with no lock at all.
        UnlockOutcome::WrongToken => no_body(out, "403 Forbidden"),
        UnlockOutcome::NotLocked => no_body(out, "409 Conflict"),
    }
}

// ---------------------------------------------------------------------------
// Probes: answered from the sidecar store, never from the peer.
// ---------------------------------------------------------------------------

pub(crate) fn put_sidecar(
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    // `docs/engine-contract.md`, item 6, I2: `PUT` honours the `If`
    // header against the lock table, sidecar or not.
    if !bridge.locks.allows(target, request.header("if")) {
        return no_body(out, "423 Locked");
    }
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

pub(crate) fn parent_of(path: &str) -> &str {
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
pub(crate) fn map_write_error(error: &RpcError) -> (&'static str, bool) {
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

/// `MKCOL` is `mkdir`. `docs/engine-contract.md`, item 6.
pub(crate) fn mkcol_verb(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    let Ok(path) = RemotePath::parse(target) else {
        return no_body(out, "404 Not Found");
    };
    if path.is_root() {
        return no_body(out, "404 Not Found");
    }
    if !bridge.locks.allows(target, request.header("if")) {
        return no_body(out, "423 Locked");
    }
    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out);
    };
    match borrowed.client().mkdir(&path) {
        Ok(()) => {
            bridge.cache.invalidate(parent_of(target));
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Mkdir,
                path.as_str(),
                None,
                None,
                None,
            );
            http::write_head(out, "201 Created", &[("Content-Length", "0".to_owned())])
        }
        // RFC 4918 9.3.1: a missing parent is 409, not `map_write_error`'s
        // ordinary 404 for `NotFound`, since here it names a missing
        // ancestor rather than the target itself. An existing target
        // still falls through to `map_write_error`, which already answers
        // `AlreadyExists` with 405.
        Err(RpcError::Remote(OpError::NotFound)) => no_body(out, "409 Conflict"),
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            no_body(out, status)
        }
    }
}

/// `DELETE`: a file is `delete`; a folder is walked with `folder.rs`'s
/// bounds and deleted leaves first, then folders deepest first, per
/// `delete::plan`. The wire stays non-recursive: one `delete` call per
/// file or folder removed. A sidecar name never reaches the peer.
pub(crate) fn delete_verb(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    if !bridge.locks.allows(target, request.header("if")) {
        return no_body(out, "423 Locked");
    }
    if probes::is_probe_name(probes::last_segment(target)) {
        return if bridge.sidecars.delete(target) {
            no_body(out, "204 No Content")
        } else {
            no_body(out, "404 Not Found")
        };
    }
    let Ok(path) = RemotePath::parse(target) else {
        return no_body(out, "404 Not Found");
    };
    if path.is_root() {
        return no_body(out, "403 Forbidden");
    }

    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out);
    };
    let entry = match borrowed.client().stat(&path) {
        Ok(entry) => entry,
        Err(RpcError::Remote(OpError::NotFound)) => return no_body(out, "404 Not Found"),
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    };

    if entry.kind == FileKind::File {
        return match borrowed.client().delete(&path) {
            Ok(()) => {
                bridge.cache.invalidate(parent_of(target));
                record_this(
                    shared,
                    &bridge.device_key_hex,
                    AccessVerb::Delete,
                    path.as_str(),
                    None,
                    None,
                    None,
                );
                no_body(out, "204 No Content")
            }
            Err(error) => {
                let (status, unhealthy) = map_write_error(&error);
                if unhealthy {
                    borrowed.mark_unhealthy();
                }
                no_body(out, status)
            }
        };
    }

    let lister = RemoteLister::new(borrowed.client());
    let plan_result = delete::plan(&lister, &path);
    let stuck_connection = lister.take_failure();
    drop(lister);
    let plan = match plan_result {
        Ok(plan) => plan,
        Err(delete::PlanError::TooLarge) => return no_body(out, "507 Insufficient Storage"),
        Err(delete::PlanError::Op(op)) => {
            let error = stuck_connection.unwrap_or(RpcError::Remote(op));
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    };

    // Every planned path is checked against the lock table before the
    // first delete: a locked file or folder anywhere in the tree refuses
    // the whole `DELETE`, with nothing removed, rather than stopping
    // partway through once a delete call happens to reach it.
    for leaf in plan.files.iter().chain(plan.dirs.iter()) {
        if !bridge.locks.allows(leaf.as_str(), request.header("if")) {
            return no_body(out, "423 Locked");
        }
    }

    let file_count = plan.file_count();
    for leaf in plan.files.iter().chain(plan.dirs.iter()) {
        if let Err(error) = borrowed.client().delete(leaf) {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    }
    bridge.cache.invalidate(parent_of(target));
    for dir in &plan.dirs {
        bridge.cache.invalidate(dir.as_str());
    }
    record_this(
        shared,
        &bridge.device_key_hex,
        AccessVerb::Delete,
        path.as_str(),
        None,
        None,
        Some(file_count),
    );
    no_body(out, "204 No Content")
}

/// `Overwrite: F`'s check, shared by `move_verb` and `copy_verb`: `None`
/// when `dest_path` does not exist on the peer, so the caller proceeds;
/// `Some` with the status to answer otherwise, 412 when it does exist and
/// whatever `map_write_error` names for any other failure.
pub(crate) fn overwrite_conflict(
    borrowed: &mut pool::Borrowed<'_>,
    dest_path: &RemotePath,
) -> Option<&'static str> {
    match borrowed.client().stat(dest_path) {
        Ok(_) => Some("412 Precondition Failed"),
        Err(RpcError::Remote(OpError::NotFound)) => None,
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            Some(status)
        }
    }
}

/// `MOVE` is `rename` within one root. `Overwrite: F` is honoured with
/// 412; across roots the peer's own refusal answers 502
/// (`map_write_error`). The destination is read from the `Destination`
/// header, with the same `Host` check and percent decoding the primary
/// target gets. A sidecar name never reaches the peer.
///
/// Either end named `.ferry-part` is 403: that name belongs to a landing
/// still in progress, never to Finder. So is a destination that is a probe
/// name while the source is not: a real file has no sidecar entry to
/// become, and letting the rename reach the peer would leave a real file
/// there under a name this bridge never shows again.
pub(crate) fn move_verb(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    from_target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    if !bridge.locks.allows(from_target, request.header("if")) {
        return no_body(out, "423 Locked");
    }
    let expected_host = format!("127.0.0.1:{}", bridge.port);
    let Some(destination) = http::destination_path(request.header("destination"), &expected_host)
    else {
        return no_body(out, "400 Bad Request");
    };
    let destination = destination.trim_matches('/');
    if !bridge.locks.allows(destination, request.header("if")) {
        return no_body(out, "423 Locked");
    }

    if put::is_partial_name(probes::last_segment(from_target))
        || put::is_partial_name(probes::last_segment(destination))
    {
        return no_body(out, "403 Forbidden");
    }

    let from_probe = probes::is_probe_name(probes::last_segment(from_target));
    let to_probe = probes::is_probe_name(probes::last_segment(destination));
    if to_probe && !from_probe {
        return no_body(out, "403 Forbidden");
    }

    if from_probe {
        return if bridge.sidecars.rename(from_target, destination) {
            no_body(out, "204 No Content")
        } else {
            no_body(out, "404 Not Found")
        };
    }

    let Ok(from_path) = RemotePath::parse(from_target) else {
        return no_body(out, "404 Not Found");
    };
    let Ok(to_path) = RemotePath::parse(destination) else {
        return no_body(out, "400 Bad Request");
    };
    let overwrite_forbidden = request.header("overwrite") == Some("F");

    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out);
    };
    if overwrite_forbidden && let Some(status) = overwrite_conflict(&mut borrowed, &to_path) {
        return no_body(out, status);
    }
    match borrowed.client().rename(&from_path, &to_path) {
        Ok(()) => {
            bridge.cache.invalidate(parent_of(from_target));
            bridge.cache.invalidate(parent_of(destination));
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Rename,
                to_path.as_str(),
                None,
                None,
                None,
            );
            no_body(out, "204 No Content")
        }
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            no_body(out, status)
        }
    }
}

/// `COPY` of a file reads it from the peer into a fresh spool file, then
/// pushes it back under the new name through [`put::land_new`], item 5's
/// landing rule. `COPY` of a folder is 403.
pub(crate) fn copy_verb(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    source_target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    let expected_host = format!("127.0.0.1:{}", bridge.port);
    let Some(destination) = http::destination_path(request.header("destination"), &expected_host)
    else {
        return no_body(out, "400 Bad Request");
    };
    let destination = destination.trim_matches('/');
    if !bridge.locks.allows(destination, request.header("if")) {
        return no_body(out, "423 Locked");
    }

    // A probe name's bytes live only in the sidecar store: the source is
    // read from there and written back under the destination name,
    // without a single byte reaching the peer.
    if probes::is_probe_name(probes::last_segment(source_target)) {
        let Some(sidecar) = bridge.sidecars.read(source_target) else {
            return no_body(out, "404 Not Found");
        };
        return match bridge.sidecars.write(destination, &sidecar.bytes) {
            Ok(()) => http::write_head(out, "201 Created", &[("Content-Length", "0".to_owned())]),
            Err(SidecarWriteError::TooLarge) => no_body(out, "413 Payload Too Large"),
            Err(SidecarWriteError::Full) => no_body(out, "507 Insufficient Storage"),
            Err(SidecarWriteError::Io) => no_body(out, "500 Internal Server Error"),
        };
    }

    let Ok(source_path) = RemotePath::parse(source_target) else {
        return no_body(out, "404 Not Found");
    };
    let Ok(dest_path) = RemotePath::parse(destination) else {
        return no_body(out, "400 Bad Request");
    };

    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out);
    };
    // `Overwrite: F` is honoured the same way `move_verb` honours it: a
    // destination that already exists is 412, checked before anything is
    // read from the source.
    if request.header("overwrite") == Some("F")
        && let Some(status) = overwrite_conflict(&mut borrowed, &dest_path)
    {
        return no_body(out, status);
    }
    let entry = match borrowed.client().stat(&source_path) {
        Ok(entry) => entry,
        Err(RpcError::Remote(OpError::NotFound)) => return no_body(out, "404 Not Found"),
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    };
    if entry.kind == FileKind::Directory {
        return no_body(out, "403 Forbidden");
    }

    let spool = match put::new_spool_path(shared, &bridge.device_key_hex, entry.size) {
        Ok(spool) => spool,
        Err(put::SpoolError::Full) => return no_body(out, "507 Insufficient Storage"),
        Err(put::SpoolError::Failed) => return no_body(out, "500 Internal Server Error"),
    };
    let mut bytes_written = 0u64;
    let landing = copy_landing(
        &mut borrowed,
        &source_path,
        &dest_path,
        spool.path(),
        entry.size,
        &mut bytes_written,
    );

    match landing {
        Ok(()) => {
            bridge.cache.invalidate(parent_of(destination));
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Read,
                source_path.as_str(),
                Some(entry.size),
                None,
                None,
            );
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Write,
                dest_path.as_str(),
                Some(entry.size),
                None,
                None,
            );
            http::write_head(out, "201 Created", &[("Content-Length", "0".to_owned())])
        }
        Err(error) => {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            no_body(out, status)
        }
    }
}

/// Reads `source` into `spool_path`, then lands it at `destination` as a
/// new file. One function so `copy_verb` never holds two overlapping
/// mutable borrows of `borrowed`'s connection at once.
pub(crate) fn copy_landing(
    borrowed: &mut pool::Borrowed<'_>,
    source: &RemotePath,
    destination: &RemotePath,
    spool_path: &std::path::Path,
    size: u64,
    bytes_written: &mut u64,
) -> Result<(), RpcError> {
    put::fetch_into_spool(borrowed.client(), source, spool_path, size)?;
    let (fs, leaf, manifest) =
        put::open_spool(spool_path).ok_or(RpcError::Remote(OpError::Internal))?;
    put::land_new(
        borrowed.client(),
        destination,
        &fs,
        &leaf,
        &manifest,
        bytes_written,
    )
}

/// `PUT` of a real file. `docs/engine-contract.md`, item 6, I2: the body
/// is spooled to disk in pieces as it arrives, never held whole in
/// memory. A new destination lands the push way
/// ([`put::land_new`]); an existing one lands the chunks that differ, in
/// place ([`put::land_delta`]), which is the delta on save.
pub(crate) fn put_file(
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
    if !bridge.locks.allows(target, head.header("if")) {
        refuse!("423 Locked");
    }
    let spool = match put::new_spool_path(shared, &bridge.device_key_hex, content_length) {
        Ok(spool) => spool,
        Err(put::SpoolError::Full) => refuse!("507 Insufficient Storage"),
        Err(put::SpoolError::Failed) => refuse!("500 Internal Server Error"),
    };

    // A short body or an I/O failure here propagates as an `Err`, closing
    // the connection: the same reasoning `stream_body`'s own S2 carries
    // for a `GET`, in the write direction. Everything from here on
    // answers a real response instead, since the body has, by this
    // point, been received in full and correctly. `spool`'s own `Drop`
    // removes the file on this path, and every path below it.
    put::spool_body(reader, content_length, spool.path())?;

    let Some((fs, leaf, manifest)) = put::open_spool(spool.path()) else {
        return no_body(out, "500 Internal Server Error").map(|()| true);
    };
    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return unavailable(out).map(|()| true);
    };
    let mut bytes_written = 0u64;
    let landing = put_landing(
        &mut borrowed,
        &path,
        &fs,
        &leaf,
        &manifest,
        &mut bytes_written,
    );

    match landing {
        Ok(kind) => {
            bridge.cache.invalidate(parent_of(target));
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Write,
                path.as_str(),
                Some(bytes_written),
                None,
                None,
            );
            let status = match kind {
                Landing::New => "201 Created",
                Landing::Delta => "204 No Content",
            };
            // The peer is asked fresh, rather than the size and time this
            // side computed, so the `ETag` always names what actually
            // landed. A failed re-stat here is not worth failing an
            // otherwise successful `PUT` over, so it just means no `ETag`.
            let etag = borrowed
                .client()
                .stat(&path)
                .ok()
                .map(|entry| http::etag(entry.size, entry.modified_unix_secs));
            let mut headers = vec![("Content-Length", "0".to_owned())];
            if let Some(etag) = &etag {
                headers.push(("ETag", etag.clone()));
            }
            http::write_head(out, status, &headers).map(|()| true)
        }
        // A `PUT` onto an existing folder's path fits no verb this
        // bridge otherwise answers with 409, so it is named here rather
        // than folded into `map_write_error`'s generic mapping.
        Err(RpcError::Remote(OpError::IsADirectory)) => no_body(out, "409 Conflict").map(|()| true),
        Err(error) => {
            // `docs/engine-contract.md`, item 6: "a failed landing removes
            // the spool file." Its access log entry still says what was
            // actually written to the peer before it failed, not nothing.
            record_this(
                shared,
                &bridge.device_key_hex,
                AccessVerb::Write,
                path.as_str(),
                Some(bytes_written),
                None,
                None,
            );
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
pub(crate) enum Landing {
    New,
    Delta,
}

/// Stats `destination` to decide which landing rule applies, then runs
/// it. One function so `put_file` never holds two overlapping mutable
/// borrows of `borrowed`'s connection at once.
pub(crate) fn put_landing(
    borrowed: &mut pool::Borrowed<'_>,
    destination: &RemotePath,
    fs: &LocalFs,
    leaf: &RemotePath,
    manifest: &Manifest,
    bytes_written: &mut u64,
) -> Result<Landing, RpcError> {
    match borrowed.client().stat(destination) {
        Ok(entry) if entry.kind == FileKind::Directory => {
            Err(RpcError::Remote(OpError::IsADirectory))
        }
        Ok(_) => {
            put::land_delta(
                borrowed.client(),
                destination,
                fs,
                leaf,
                manifest,
                bytes_written,
            )?;
            Ok(Landing::Delta)
        }
        Err(RpcError::Remote(OpError::NotFound)) => {
            put::land_new(
                borrowed.client(),
                destination,
                fs,
                leaf,
                manifest,
                bytes_written,
            )?;
            Ok(Landing::New)
        }
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Real paths: served through the pool.
// ---------------------------------------------------------------------------

/// Reads the heads of one listing's images into the head cache, one
/// listing at a time, until `running` clears.
///
/// `docs/engine-contract.md`, item 17. One of these runs per bridge,
/// started and joined by `MountRegistry`.
pub(crate) fn prefetch_loop(shared: &Arc<Shared>, bridge: &Arc<Bridge>, running: &Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        let Some(job) = bridge.prefetch.next() else {
            return;
        };
        prefetch_job(shared, bridge, running, &job);
    }
}

/// Reads the head of every image in one listing that is not cached
/// already, and leaves one `Read` entry in this side's access log for the
/// whole listing.
///
/// Each file is read under its own pool borrow, so the prefetch holds at
/// most one of the bridge's four connections and Finder's own requests
/// interleave with it. `running` is read between files, and again on every
/// pass of [`read_head_bounded`], so `MountRegistry::stop` waits for one
/// read and not for a whole folder.
pub(crate) fn prefetch_job(
    shared: &Arc<Shared>,
    bridge: &Arc<Bridge>,
    running: &Arc<AtomicBool>,
    job: &heads::Job,
) {
    let mut files = 0u32;
    let mut bytes = 0u64;
    for file in &job.files {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if !heads::is_image(&file.name) {
            continue;
        }
        if bridge
            .heads
            .holds(&file.path, file.size, file.modified_unix_secs)
        {
            continue;
        }
        let want = file.size.min(heads::HEAD_LEN);
        if want == 0 {
            continue;
        }
        let Ok(path) = RemotePath::parse(&file.path) else {
            continue;
        };
        let Ok(mut borrowed) = bridge.pool.take(shared) else {
            // The device is not reachable. Nothing else in this listing
            // will read either, so the rest of it is dropped rather than
            // tried file by file.
            break;
        };
        let head = read_head(&mut borrowed, &path, want, running);
        drop(borrowed);
        // A head shorter than the listing said means the file changed
        // under the prefetch. Its new size and time are its own key, so
        // there is nothing here worth storing.
        let Some(head) = head.filter(|head| u64::try_from(head.len()) == Ok(want)) else {
            continue;
        };
        bytes = bytes.saturating_add(want);
        files = files.saturating_add(1);
        bridge
            .heads
            .put(&file.path, file.size, file.modified_unix_secs, head);
    }
    // Item 17: the prefetch of one listing is one `Read` entry on this
    // side, naming the folder, the bytes read, and the file count. A
    // listing that read nothing leaves no entry, since there is nothing
    // true to say about the wire.
    if files > 0 {
        record_this(
            shared,
            &bridge.device_key_hex,
            AccessVerb::Read,
            &job.folder,
            Some(bytes),
            None,
            Some(files),
        );
    }
}

/// Reads the first `want` bytes of `path` from the peer. `None` when the
/// peer's read fails, which marks the borrowed connection unhealthy the
/// same way [`stream_body`] does, and `None` when `running` clears while
/// the read is in flight.
pub(crate) fn read_head(
    borrowed: &mut pool::Borrowed<'_>,
    path: &RemotePath,
    want: u64,
    running: &AtomicBool,
) -> Option<Vec<u8>> {
    read_head_bounded(want, running, |offset, ask| {
        match borrowed.client().read(path, offset, ask) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                let (_, unhealthy) = map_rpc_error(&error);
                if unhealthy {
                    borrowed.mark_unhealthy();
                }
                None
            }
        }
    })
}

/// The read loop [`read_head`] runs, with the peer behind a closure so a
/// test can answer it without a socket.
///
/// `docs/engine-contract.md`, item 17. At most
/// [`ferry_core::limits::MAX_READS_PER_CHUNK`] reads, so a peer that
/// answers one byte at a time cannot hold this loop for as many round
/// trips as the head has bytes. `running` is read on every pass, so
/// `MountRegistry::stop` waits for one read and not for a whole head.
///
/// A head shorter than `want` is still returned. Its caller keeps only a
/// head of exactly the length the listing promised, so a short one is
/// dropped there rather than cached as if it were whole.
pub(crate) fn read_head_bounded(
    want: u64,
    running: &AtomicBool,
    mut read: impl FnMut(u64, u32) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut head: Vec<u8> = Vec::new();
    for _ in 0..ferry_core::limits::MAX_READS_PER_CHUNK {
        if !running.load(Ordering::SeqCst) {
            return None;
        }
        let read_so_far = u64::try_from(head.len()).unwrap_or(want);
        if read_so_far >= want {
            break;
        }
        let ask = u32::try_from(want - read_so_far)
            .unwrap_or(ferry_core::limits::MAX_READ_LEN)
            .min(ferry_core::limits::MAX_READ_LEN);
        let bytes = read(read_so_far, ask)?;
        if bytes.is_empty() {
            break;
        }
        head.extend_from_slice(&bytes);
    }
    Some(head)
}

/// What to answer for one failed operation on the peer, and whether the
/// connection that produced it should be dropped rather than reused.
pub(crate) fn map_rpc_error(error: &RpcError) -> (&'static str, bool) {
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use ferry_core::limits::MAX_READS_PER_CHUNK;

    use super::read_head_bounded;

    /// `docs/engine-contract.md`, item 17, and the third-run engine audit,
    /// finding 3. A peer that answers one byte per read would otherwise
    /// make the loop run one round trip per byte of the head: 65536 of
    /// them for one 64 KiB head.
    #[test]
    fn a_peer_that_answers_one_byte_at_a_time_is_bounded() {
        let running = AtomicBool::new(true);
        let mut calls = 0u32;
        let head = read_head_bounded(64 * 1024, &running, |_offset, _ask| {
            calls += 1;
            Some(vec![0u8; 1])
        })
        .expect("a peer that answers every read is not the failure path");
        assert_eq!(
            calls, MAX_READS_PER_CHUNK,
            "the loop must stop after MAX_READS_PER_CHUNK reads"
        );
        assert_eq!(
            head.len(),
            MAX_READS_PER_CHUNK as usize,
            "one byte per read, so the short head holds one byte per read made"
        );
    }

    /// The same finding's second half. `MountRegistry::stop` clears
    /// `running`, and the loop must read it on every pass rather than only
    /// between whole files.
    #[test]
    fn a_cleared_running_flag_ends_the_read_loop() {
        let running = AtomicBool::new(true);
        let mut calls = 0u32;
        let head = read_head_bounded(64 * 1024, &running, |_offset, _ask| {
            calls += 1;
            running.store(false, Ordering::SeqCst);
            Some(vec![0u8; 1])
        });
        assert!(
            head.is_none(),
            "a head the bridge stopped collecting is not a head to cache"
        );
        assert_eq!(calls, 1, "the flag is read before every read, not once");
    }
}
