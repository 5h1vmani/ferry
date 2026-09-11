//! `LOCK` and `UNLOCK`. Neither ever reaches the peer.

use std::io::{self, Write};

use crate::dav::http;
use crate::dav::lock::{LockError, UnlockOutcome};
use crate::dav::server::{Bridge, no_body};

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
