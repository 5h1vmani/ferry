//! The accept loop, the per-connection loop, and the verbs.
//!
//! `docs/engine-contract.md`, item 6, I1: `OPTIONS`; `PROPFIND` at depth 0
//! and 1; `GET` and `HEAD`, with one `Range`; `LOCK` and `UNLOCK`; and a
//! `PUT` of a sidecar name. Every other write verb answers 403; item I2
//! builds `PUT` of a real file, `DELETE`, `MOVE`, `MKCOL`, `COPY`, and
//! `PROPPATCH`.

use std::io::{self, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use ferry_core::noise::SecureStream;
use ferry_core::ops::{Entry, FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, RpcError};

use crate::engine::Shared;
use crate::guard::StopAware;

use super::cache::Cache;
use super::lock::LockTable;
use super::pool::Pool;
use super::probes::{self, SidecarStore};
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
        user: String,
        password: String,
        port: u16,
        sidecar_dir: PathBuf,
    ) -> Self {
        Self {
            pool: Pool::new(device_key_hex),
            sidecars: SidecarStore::new(sidecar_dir),
            cache: Cache::new(),
            locks: LockTable::new(),
            connections: Arc::new(AtomicU32::new(0)),
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
        let Ok(outcome) = http::read_request(&mut reader) else {
            return;
        };
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        let request = match outcome {
            http::ReadOutcome::Request(request) => request,
            http::ReadOutcome::Closed => return,
            // B2: the head ran past its budget, or carried too many
            // headers. There is no safe place left to resume parsing the
            // next request from, so this answers once and closes.
            http::ReadOutcome::HeadTooLarge => {
                let _ = no_body(&mut out, "431 Request Header Fields Too Large");
                return;
            }
            // B1: `Content-Length` claimed more than this bridge will
            // allocate for, checked before a single byte of the body was
            // read. Closing rather than continuing: the peer's declared
            // body is still sitting unread on the wire, and would be
            // misread as the start of the next request.
            http::ReadOutcome::BodyTooLarge => {
                let _ = no_body(&mut out, "413 Payload Too Large");
                return;
            }
        };
        if respond(shared, bridge, &request, &mut out).is_err() {
            return;
        }
        if out.flush().is_err() {
            return;
        }
    }
}

/// Answers one request: `Host` and Basic auth first, then the verb.
fn respond(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    let expected_host = format!("127.0.0.1:{}", bridge.port);
    if request.header("host") != Some(expected_host.as_str()) {
        return no_body(out, "400 Bad Request");
    }
    if !authorized(bridge, request) {
        return http::write_head(
            out,
            "401 Unauthorized",
            &[
                ("WWW-Authenticate", "Basic realm=\"Ferry\"".to_owned()),
                ("Content-Length", "0".to_owned()),
            ],
        );
    }
    if request.method == "OPTIONS" {
        return options(out);
    }

    let decoded = http::percent_decode(&request.target);
    let target = decoded.trim_start_matches('/').trim_end_matches('/');
    let is_probe = probes::is_probe_name(probes::last_segment(target));

    match request.method.as_str() {
        "LOCK" => lock_verb(bridge, target, out),
        "UNLOCK" => unlock_verb(bridge, request, target, out),
        "PUT" if is_probe => put_sidecar(bridge, target, request, out),
        "PROPFIND" if is_probe => propfind_probe(bridge, target, request, out),
        "GET" | "HEAD" if is_probe => get_probe(bridge, target, &request.method, out),
        "PROPFIND" => propfind(shared, bridge, target, request, out),
        "GET" | "HEAD" => get_file(shared, bridge, target, &request.method, request, out),
        // I2 builds these. `docs/engine-contract.md`, item 6.
        "PUT" | "DELETE" | "MOVE" | "MKCOL" | "COPY" | "PROPPATCH" => no_body(out, "403 Forbidden"),
        _ => no_body(out, "405 Method Not Allowed"),
    }
}

fn no_body(out: &mut impl Write, status: &str) -> io::Result<()> {
    http::write_head(out, status, &[("Content-Length", "0".to_owned())])
}

fn unavailable(out: &mut impl Write) -> io::Result<()> {
    no_body(out, "503 Service Unavailable")
}

/// Checks the `Authorization` header against `bridge`'s per-start
/// password. `bridge.user` is checked in the ordinary way; only the
/// password compare needs to be constant-time, since the user name is
/// fixed and not a secret.
fn authorized(bridge: &Bridge, request: &http::Request) -> bool {
    let Some(header) = request.header("authorization") else {
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

fn options(out: &mut impl Write) -> io::Result<()> {
    http::write_head(
        out,
        "200 OK",
        &[
            (
                "Allow",
                "OPTIONS, GET, HEAD, PUT, PROPFIND, LOCK, UNLOCK".to_owned(),
            ),
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
    let Ok(token) = bridge.locks.lock_path(target) else {
        return no_body(out, "500 Internal Server Error");
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
    if bridge.sidecars.write(target, &request.body).is_err() {
        return no_body(out, "500 Internal Server Error");
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
// Real paths: served through the pool.
// ---------------------------------------------------------------------------

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
    let depth = request.header("depth").unwrap_or("infinity");

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

    if depth != "0" && self_entry.kind == FileKind::Directory {
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
            if probes::is_probe_name(&child.name) {
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

    let props = xml::requested_props(&request.body);
    let items: Vec<xml::Item<'_>> = named
        .iter()
        .map(|(path, entry)| xml::Item {
            path,
            name: if path.is_empty() {
                ""
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

    let range = request
        .header("range")
        .and_then(|value| http::parse_range(value, entry.size));
    let (start, end) = range.unwrap_or((0, entry.size.saturating_sub(1)));
    let len = if entry.size == 0 {
        0
    } else {
        end.saturating_sub(start) + 1
    };

    let status = if range.is_some() {
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
    if range.is_some() {
        headers.push((
            "Content-Range",
            format!("bytes {start}-{end}/{}", entry.size),
        ));
    }
    http::write_head(out, status, &headers)?;

    if method == "HEAD" || len == 0 {
        return Ok(());
    }

    // Served in pieces of at most one mebibyte through the peer's `read`,
    // whether or not a `Range` was asked for, per
    // `docs/engine-contract.md`, item 6, and stops at the first failed
    // write to the socket: `docs/spike-0-findings.md`, question 2, found
    // macOS aborts an open-ended range early, and a naive server that
    // tried to hand it a whole 512 MiB file at once wasted the read.
    let piece = ferry_core::limits::MAX_READ_LEN;
    let mut offset = start;
    let stop_at = end.saturating_add(1);
    while offset < stop_at {
        let want = u32::try_from((stop_at - offset).min(u64::from(piece))).unwrap_or(piece);
        let bytes = match borrowed.client().read(&path, offset, want) {
            Ok(bytes) => bytes,
            Err(error) => {
                let (_, unhealthy) = map_rpc_error(&error);
                if unhealthy {
                    borrowed.mark_unhealthy();
                }
                return Ok(());
            }
        };
        if bytes.is_empty() {
            break;
        }
        if out.write_all(&bytes).is_err() {
            return Ok(());
        }
        offset += u64::try_from(bytes.len()).unwrap_or(0);
    }
    Ok(())
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
