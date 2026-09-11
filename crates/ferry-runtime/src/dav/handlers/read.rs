//! `GET` and `HEAD`: reading a file, from the head cache or the wire.

use std::io::{self, Write};
use std::sync::Arc;

use ferry_core::ops::{Entry, FileKind};
use ferry_core::path::RemotePath;

use crate::access::AccessVerb;
use crate::engine::{Shared, record_this};
use crate::pool::{self};

use crate::dav::http;
use crate::dav::server::{Bridge, map_rpc_error, no_body};

pub(crate) fn get_probe(
    bridge: &Bridge,
    target: &str,
    method: &str,
    out: &mut impl Write,
) -> io::Result<()> {
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

pub(crate) fn get_file(
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

    let (entry, borrowed) = match file_entry(shared, bridge, target, &path, method, request) {
        Ok(found) => found,
        Err(status) => return no_body(out, status),
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
        let body = BodyPlan {
            target,
            entry: &entry,
            path: &path,
            start,
            end,
        };
        send_body(shared, bridge, &body, borrowed, out)?
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

/// What `GET` and `HEAD` need to know about the file: its entry, and the
/// pooled connection the answer already holds, if it took one.
///
/// Item 17: an answer whose body comes entirely from the head cache takes
/// the file's size and modified time from the parent folder's listing, so
/// a thumbnail request that follows its own listing touches the wire for
/// nothing. An answer that needs any byte from the wire stats on the wire
/// first, and that borrow is handed back to the caller, which still needs
/// it for the body. So the head is used only when the fresh size and time
/// match the head's key, and a file replaced since the listing is streamed
/// whole from the wire as it was before item 17.
///
/// # Errors
///
/// Returns the status to answer with when the device is unreachable or the
/// peer refused the `stat`.
pub(crate) fn file_entry<'a>(
    shared: &Arc<Shared>,
    bridge: &'a Bridge,
    target: &str,
    path: &RemotePath,
    method: &str,
    request: &http::Request,
) -> Result<(Entry, Option<pool::Borrowed<'a>>), &'static str> {
    if let Some(entry) = listed_entry(bridge, target)
        && !needs_the_wire(bridge, target, &entry, method, request)
    {
        return Ok((entry, None));
    }
    let Ok(mut borrowed) = bridge.pool.take(shared) else {
        return Err("503 Service Unavailable");
    };
    match borrowed.client().stat(path) {
        Ok(entry) => Ok((entry, Some(borrowed))),
        Err(error) => {
            let (status, unhealthy) = map_rpc_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            Err(status)
        }
    }
}

/// True when the answer this request asks for needs at least one byte the
/// cached head does not hold, so the listing's size and time cannot be
/// trusted for it.
///
/// `docs/engine-contract.md`, item 17. `entry` is the listing's own entry,
/// which may be up to two seconds old. A `HEAD`, an empty file, and a range
/// this side cannot satisfy all send no body, so none of them needs the
/// wire. Everything else needs the wire unless the head holds the last byte
/// the response promises.
pub(crate) fn needs_the_wire(
    bridge: &Bridge,
    target: &str,
    entry: &Entry,
    method: &str,
    request: &http::Request,
) -> bool {
    if method == "HEAD" || entry.size == 0 {
        return false;
    }
    let outcome = request
        .header("range")
        .map_or(http::RangeOutcome::Absent, |value| {
            http::parse_range(value, entry.size)
        });
    // The last byte the response promises. The head always starts at zero,
    // so the head holds the whole answer exactly when it reaches this byte.
    let end = match outcome {
        http::RangeOutcome::Unsatisfiable => return false,
        http::RangeOutcome::Absent => entry.size.saturating_sub(1),
        http::RangeOutcome::Satisfiable(_, end) => end,
    };
    let cached_bytes = bridge
        .heads
        .get(target, entry.size, entry.modified_unix_secs)
        .map_or(0, |head| u64::try_from(head.len()).unwrap_or(0));
    end >= cached_bytes
}

/// Writes `[start, end]` of the file, and answers how many bytes went out.
///
/// Item 17: a range that starts inside the cached head, or a whole file,
/// is served from the head for as many bytes as the head holds. Only what
/// is left goes to the wire, on `borrowed` when `file_entry` already took
/// one, and on a fresh borrow otherwise.
///
/// # Errors
///
/// Returns an error when the peer's read fails or the write to `out`
/// fails. The response head, with its `Content-Length`, is already
/// written by then, so there is no status left to answer with: S2 has
/// `handle_connection` drop the connection instead.
pub(crate) fn send_body(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    body: &BodyPlan<'_>,
    borrowed: Option<pool::Borrowed<'_>>,
    out: &mut impl Write,
) -> io::Result<u64> {
    let from_head = write_cached_head(bridge, body, out)?;
    let next = body.start.saturating_add(from_head);
    if next > body.end {
        return Ok(from_head);
    }
    let mut borrowed = match borrowed {
        Some(borrowed) => borrowed,
        None => bridge.pool.take(shared).map_err(|_| {
            io::Error::other("the device went away before the declared Content-Length was met")
        })?,
    };
    let streamed = stream_body(
        &mut borrowed,
        body.path,
        next,
        body.end.saturating_add(1),
        out,
    )?;
    Ok(from_head.saturating_add(streamed))
}

/// One `GET`'s body: which file, and which bytes of it the response head
/// already promised.
pub(crate) struct BodyPlan<'a> {
    /// The DAV target, which is also the head cache key.
    target: &'a str,
    /// What the listing or the `stat` said the file is.
    entry: &'a Entry,
    path: &'a RemotePath,
    start: u64,
    /// The last byte to send, not one past it.
    end: u64,
}

/// The file's own entry from its parent folder's cached listing, when that
/// listing is still within its two second TTL.
///
/// `docs/engine-contract.md`, item 17: `GET` and `HEAD` take the file's
/// size and modified time from the listing cache, and stat on the wire
/// only otherwise. The listing cache drops a folder on any write through
/// this bridge to it, so a file this answers for is one nothing here has
/// changed since the listing.
pub(crate) fn listed_entry(bridge: &Bridge, target: &str) -> Option<Entry> {
    let (parent, name) = target.rsplit_once('/').unwrap_or(("", target));
    let children = bridge.cache.get(parent)?;
    children.into_iter().find(|child| child.name == name)
}

/// Writes as much of the planned range as the cached head of the file
/// holds, and answers how many bytes that was. Zero when nothing is cached
/// for this exact size and modified time, or when the range starts past
/// the end of the head.
///
/// `docs/engine-contract.md`, item 17.
///
/// # Errors
///
/// Returns an error when the write to `out` fails, the same as
/// [`stream_body`].
pub(crate) fn write_cached_head(
    bridge: &Bridge,
    body: &BodyPlan<'_>,
    out: &mut impl Write,
) -> io::Result<u64> {
    let Some(head) = bridge
        .heads
        .get(body.target, body.entry.size, body.entry.modified_unix_secs)
    else {
        return Ok(0);
    };
    let cached_bytes = u64::try_from(head.len()).unwrap_or(0);
    if body.start >= cached_bytes {
        return Ok(0);
    }
    let stop_at = body.end.saturating_add(1).min(cached_bytes);
    let from = usize::try_from(body.start).unwrap_or(usize::MAX);
    let to = usize::try_from(stop_at).unwrap_or(usize::MAX);
    out.write_all(&head[from..to])?;
    Ok(stop_at - body.start)
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
pub(crate) fn stream_body(
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
