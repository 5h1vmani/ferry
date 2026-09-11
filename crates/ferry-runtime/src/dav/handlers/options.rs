//! `OPTIONS`, and the Basic auth check every other verb goes through.

use std::io::{self, Write};

use crate::dav::http;
use crate::dav::server::Bridge;

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
