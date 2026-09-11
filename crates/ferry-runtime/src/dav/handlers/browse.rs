//! `PROPFIND` and `PROPPATCH`: what Finder asks for when it lists a
//! folder, and the sidecar probes it asks for alongside.

use crate::dav::errors::map_rpc_error;

use crate::dav::handlers::map_write_error;

use std::io::{self, Write};
use std::sync::Arc;

use ferry_core::noise::SecureStream;
use ferry_core::ops::{Entry, FileKind};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, RpcError};

use crate::access::AccessVerb;
use crate::engine::{Shared, record_this};
use crate::guard::StopAware;

use crate::dav::probes::{self};
use crate::dav::put;
use crate::dav::server::{Bridge, no_body, unavailable};
use crate::dav::{http, xml};

pub(crate) fn propfind_probe(
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

/// `PROPPATCH` sets the modified time when `getlastmodified` or the
/// Apple `Win32LastModifiedTime` property is given, both carrying the
/// same `rfc1123` shape a well behaved client only ever echoes back. It
/// answers 200 for that property and 403 for every other, in one
/// multistatus. `set_mtime` is never logged as its own `This` entry, the
/// same rule `docs/engine-contract.md`, item 13, states for every other
/// caller of it: it always follows a write that already is, and here
/// there is no such write to follow. A sidecar name never reaches the
/// peer.
pub(crate) fn proppatch_verb(
    shared: &Arc<Shared>,
    bridge: &Bridge,
    target: &str,
    request: &http::Request,
    out: &mut impl Write,
) -> io::Result<()> {
    if !bridge.locks.allows(target, request.header("if")) {
        return no_body(out, "423 Locked");
    }
    let parsed = xml::read_proppatch(&request.body);
    let mtime = parsed.mtime_text.as_deref().and_then(http::parse_rfc1123);
    // A modified time property whose date failed to parse is refused, the
    // same as any other property this bridge does not set: reporting it
    // as accepted would tell the client its date landed when nothing was
    // ever touched.
    let (accepted, refused): (Vec<String>, Vec<String>) =
        parsed.names.into_iter().partition(|name| {
            let lower = name.to_ascii_lowercase();
            (lower == "getlastmodified" || lower == "win32lastmodifiedtime") && mtime.is_some()
        });

    if probes::is_probe_name(probes::last_segment(target)) {
        if let Some(when) = mtime {
            bridge.sidecars.set_mtime(target, when);
        }
        let body = xml::proppatch_multistatus(target, false, &accepted, &refused);
        return write_multistatus_xml(out, &body);
    }

    let Ok(path) = RemotePath::parse(target) else {
        return no_body(out, "404 Not Found");
    };
    if let Some(when) = mtime {
        let Ok(mut borrowed) = bridge.pool.take(shared) else {
            return unavailable(out);
        };
        if let Err(error) = borrowed.client().set_mtime(&path, when) {
            let (status, unhealthy) = map_write_error(&error);
            if unhealthy {
                borrowed.mark_unhealthy();
            }
            return no_body(out, status);
        }
    }
    let body = xml::proppatch_multistatus(target, false, &accepted, &refused);
    write_multistatus_xml(out, &body)
}

pub(crate) fn write_multistatus_xml(out: &mut impl Write, body: &str) -> io::Result<()> {
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

/// N3, RFC 4918 9.1: a `PROPFIND` this bridge will not walk. `depth` is
/// `None` for a missing header and `Some("infinity")` for an explicit one;
/// both mean "the whole tree", which I1 never serves.
pub(crate) fn depth_not_finite(out: &mut impl Write) -> io::Result<()> {
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

pub(crate) fn propfind(
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
    let written = write_multistatus(out, &items, &props);

    // Item 17: the response is out, so the person is already looking at
    // the folder. Hand its children to the prefetch thread, which is what
    // makes the thumbnail requests that follow cost nothing on the wire.
    // `named[0]` is the folder itself, which has no head to read.
    if listed {
        bridge.prefetch.submit(path.as_str(), &named[1..]);
    }
    written
}

pub(crate) fn write_multistatus(
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
pub(crate) fn list_all(
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
