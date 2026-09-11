//! The write verbs: `PUT`, `MKCOL`, `DELETE`, `MOVE`, and `COPY`.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use ferry_core::chunk::Manifest;
use ferry_core::localfs::LocalFs;
use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::RpcError;

use crate::access::AccessVerb;
use crate::engine::{Shared, record_this};
use crate::folder::RemoteLister;
use crate::pool::{self};

use crate::dav::delete;
use crate::dav::http;
use crate::dav::probes::{self, SidecarWriteError};
use crate::dav::put;
use crate::dav::server::{Bridge, no_body, unavailable};

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
