//! A hand-written blocking HTTP/1.1 request parser and response writer, and
//! the two date shapes `docs/engine-contract.md`, item 6, needs: RFC 1123
//! for `getlastmodified` and the `Last-Modified` header, and an ISO 8601
//! stamp for `creationdate`.
//!
//! No date crate is a dependency of this workspace, and item 6 asks for no
//! new one, so the Unix-seconds-to-civil-date conversion below is written
//! out. It is Howard Hinnant's `civil_from_days` algorithm, the same one
//! most date libraries use under their own APIs; the tests check it against
//! known dates rather than trusting the arithmetic on sight.

use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};

use crate::state::now_unix_secs;

/// The per-request head budget: the request line plus every header line,
/// read through a [`Read::take`] wrapper so a peer that never sends a
/// newline cannot grow either buffer past this many bytes. A stranger on
/// loopback needs no password to reach this far, so the bound applies
/// before `respond` ever looks at `Authorization`.
const MAX_HEAD_LEN: u64 = 64 * 1024;

/// How many header lines one request may send. Checked separately from
/// [`MAX_HEAD_LEN`] because a request could otherwise stay under the byte
/// budget with thousands of one-byte header lines.
const MAX_HEADERS: usize = 64;

/// The largest `Content-Length` this bridge will allocate for, checked
/// before the allocation is made and before auth is checked. Well past
/// anything I1 browsing sends (a `PROPFIND` body and a sidecar `PUT` are
/// both small); `server.rs` and `probes.rs` apply their own, tighter bound
/// to a sidecar's actual bytes once the body is in hand.
///
/// A `PUT` of a real file uses [`MAX_PUT_BODY_LEN`] instead: this bound is
/// for everything else, which this bridge always reads whole into memory.
pub(crate) const MAX_BODY_LEN: u64 = 256 * 1024;

/// The largest body a `PUT` of a real file may carry.
///
/// `docs/engine-contract.md`, item 6, I2: the body is streamed to the
/// spool file in pieces and never held whole in memory, so this bound is
/// not about memory. It matches the largest file whose manifest, built at
/// the spool's own one mebibyte chunk size
/// ([`ferry_core::localfs::LocalFs::manifest`]), still fits
/// [`ferry_core::limits::MAX_MANIFEST_CHUNKS`]: one mebibyte times that
/// many chunks. A body any larger could spool successfully but could
/// never land, since its own manifest would refuse to decode once sent to
/// the peer, so it is refused here instead, before a single byte reaches
/// the spool file.
pub(crate) const MAX_PUT_BODY_LEN: u64 = 32 * 1024 * 1024 * 1024;

/// One parsed request line and its headers, with the body not yet read.
///
/// Header names are lowercased on the way in, so a caller never has to
/// guess a peer's capitalisation.
pub(crate) struct RequestHead {
    pub(crate) method: String,
    /// The request target exactly as sent, still percent-encoded. The
    /// caller decodes it once, per `docs/engine-contract.md`, item 6.
    pub(crate) target: String,
    pub(crate) headers: HashMap<String, String>,
}

impl RequestHead {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// The `Content-Length` header's value, or `None` when it is absent or
    /// is not a plain number. A `PUT` of a real file treats either of
    /// those the same way: 411, since there is no other way to know how
    /// much body follows.
    pub(crate) fn content_length(&self) -> Option<u64> {
        self.header("content-length")?.parse().ok()
    }
}

/// One request's method, headers, and body, once its target has already
/// been read, decoded, and routed on. Every verb function takes the
/// target as its own `&str` parameter instead, so it is not repeated
/// here.
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// What [`read_head`] found on the wire.
pub(crate) enum HeadOutcome {
    /// A full request line and header block, within every bound below.
    Head(RequestHead),
    /// The connection closed before a request line arrived. The ordinary
    /// end of a connection between requests, not a fault.
    Closed,
    /// The request line or the headers ran past [`MAX_HEAD_LEN`], or there
    /// were more than [`MAX_HEADERS`] of them. The caller answers 431 and
    /// closes the connection: with the head only partly read, there is no
    /// safe place left to resume parsing the next request from.
    HeadTooLarge,
}

/// Reads one request line and its headers, refusing to grow a buffer past
/// the bounds `docs/engine-contract.md`, item 6's threat model requires:
/// every process on this Mac can reach this loopback port, and only the
/// per-start password tells them apart from Finder, so nothing before
/// that password check may cost unbounded memory.
///
/// The body is deliberately not read here. `server::respond` reads it
/// afterward, once it knows from the method and the target whether to
/// buffer it whole (every route but a `PUT` of a real file) or stream it
/// straight to a spool file (`docs/engine-contract.md`, item 6, I2).
pub(crate) fn read_head(reader: &mut impl BufRead) -> io::Result<HeadOutcome> {
    let mut limited = Read::take(reader, MAX_HEAD_LEN);

    let mut request_line = String::new();
    let read = limited.read_line(&mut request_line)?;
    if read == 0 {
        return Ok(HeadOutcome::Closed);
    }
    if !request_line.ends_with('\n') {
        return Ok(HeadOutcome::HeadTooLarge);
    }
    let request_line = request_line.trim_end();
    if request_line.is_empty() {
        return Ok(HeadOutcome::Closed);
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("/").to_owned();

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        let read = limited.read_line(&mut line)?;
        if read == 0 {
            // A connection that closes mid-headers has sent no request
            // this side can answer.
            return Ok(HeadOutcome::Closed);
        }
        if !line.ends_with('\n') {
            return Ok(HeadOutcome::HeadTooLarge);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Ok(HeadOutcome::HeadTooLarge);
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    Ok(HeadOutcome::Head(RequestHead {
        method,
        target,
        headers,
    }))
}

/// What [`read_bounded_body`] found.
pub(crate) enum BodyOutcome {
    /// The body, read whole.
    Body(Vec<u8>),
    /// `content_length` was over `max`. Nothing was allocated for it.
    TooLarge,
}

/// Reads exactly `content_length` bytes of body into memory, bounded by
/// `max`, checked before a single byte is allocated.
pub(crate) fn read_bounded_body(
    reader: &mut impl BufRead,
    content_length: u64,
    max: u64,
) -> io::Result<BodyOutcome> {
    if content_length > max {
        return Ok(BodyOutcome::TooLarge);
    }
    let len = usize::try_from(content_length).unwrap_or(usize::MAX);
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(BodyOutcome::Body(body))
}

/// Copies exactly `content_length` bytes of request body from `reader` to
/// `sink`, in bounded pieces, never holding more than one piece in
/// memory. Returns the number of bytes actually copied, which is less
/// than `content_length` only when the connection ended early.
pub(crate) fn copy_body(
    reader: &mut impl BufRead,
    content_length: u64,
    sink: &mut impl Write,
) -> io::Result<u64> {
    let mut limited = Read::take(reader, content_length);
    io::copy(&mut limited, sink)
}

/// Writes a status line and headers, plus a fresh `Date` header and the
/// `DAV:` capability header every response carries. Does not write a body;
/// the caller writes one afterward, in one piece or streamed.
pub(crate) fn write_head(
    out: &mut impl Write,
    status: &str,
    headers: &[(&str, String)],
) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\n");
    head.push_str("Date: ");
    head.push_str(&rfc1123(now_unix_secs()));
    head.push_str("\r\nServer: ferry\r\nDAV: 1, 2\r\n");
    for (key, value) in headers {
        head.push_str(key);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())
}

/// Percent-decodes a URL path once, as `docs/engine-contract.md`, item 6,
/// requires. An invalid escape is passed through as the literal `%XX`
/// bytes, which then fails `RemotePath::parse` or matches no route, rather
/// than being silently dropped.
pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a `Range` header means against an entity of `total` bytes.
pub(crate) enum RangeOutcome {
    /// No `Range` header, or one this side does not understand as
    /// `bytes=...`: serve the whole entity, 200.
    Absent,
    /// A `bytes=` range that cannot be satisfied against `total` bytes: a
    /// start past the end, an empty suffix such as `bytes=-0`, or a span
    /// that is backwards once its end is clamped to the entity's last
    /// byte. RFC 7233: 416, with `Content-Range: bytes */<total>`, and
    /// never a 200 with the wrong length.
    Unsatisfiable,
    /// A satisfiable inclusive span, already clamped to `total`.
    Satisfiable(u64, u64),
}

/// Reads a `Range: bytes=...` value against a file of `total` bytes. See
/// [`RangeOutcome`] for what each case means.
pub(crate) fn parse_range(value: &str, total: u64) -> RangeOutcome {
    let Some(spec) = value.strip_prefix("bytes=") else {
        return RangeOutcome::Absent;
    };
    let Some((a, b)) = spec.split_once('-') else {
        return RangeOutcome::Absent;
    };
    let (start, end) = if a.trim().is_empty() {
        let Ok(suffix) = b.trim().parse::<u64>() else {
            return RangeOutcome::Absent;
        };
        // `bytes=-0` asks for the last zero bytes: nothing satisfies that.
        if suffix == 0 {
            return RangeOutcome::Unsatisfiable;
        }
        (total.saturating_sub(suffix), total.saturating_sub(1))
    } else {
        let Ok(start) = a.trim().parse::<u64>() else {
            return RangeOutcome::Absent;
        };
        let end = if b.trim().is_empty() {
            total.saturating_sub(1)
        } else {
            match b.trim().parse::<u64>() {
                Ok(end) => end,
                Err(_) => return RangeOutcome::Absent,
            }
        };
        (start, end)
    };
    let end = end.min(total.saturating_sub(1));
    // `start >= total` catches a start past the end even when `total` is
    // 0, since a zero-length entity satisfies no range at all; `start >
    // end` catches a span that is backwards once `end` is clamped, which
    // a start past the end always is.
    if start >= total || start > end {
        return RangeOutcome::Unsatisfiable;
    }
    RangeOutcome::Satisfiable(start, end)
}

/// `"<size>-<mtime>"`, quoted, per `docs/engine-contract.md`, item 6.
pub(crate) fn etag(size: u64, modified_unix_secs: i64) -> String {
    format!("\"{size}-{modified_unix_secs}\"")
}

/// Reads a `Destination` header the way `docs/engine-contract.md`, item 6,
/// asks for `MOVE` and `COPY`: "the same `Host` check and percent
/// decoding" the primary request target already gets. `value` is the
/// whole header, an absolute URI such as
/// `"http://127.0.0.1:<port>/Root/New%20Name.txt"`. Returns the decoded,
/// root relative path, or `None` when the header is missing or names a
/// host other than `expected_host`.
pub(crate) fn destination_path(value: Option<&str>, expected_host: &str) -> Option<String> {
    let value = value?;
    let after_scheme = value.split_once("://").map_or(value, |(_, rest)| rest);
    let (host, path) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));
    if host != expected_host {
        return None;
    }
    Some(percent_decode(path))
}

/// Decodes standard base64, ignoring `=` padding. `None` on any byte
/// outside the alphabet, which an `Authorization` header never sends when
/// it means Basic auth honestly.
///
/// No base64 crate is a dependency of this workspace, so this is written
/// out, the same choice `docs/engine-contract.md`, item 6, makes for the
/// rest of the bridge.
pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in input
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
    {
        let digit = value(byte)?;
        buffer = (buffer << 6) | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xFF).unwrap_or(0));
        }
    }
    Some(out)
}

/// Compares two byte strings without branching on where they first differ,
/// so a timing measurement cannot narrow down a guessed password one byte
/// at a time. `docs/engine-contract.md`, item 6: "a constant-time compare
/// against the per-start password."
///
/// The early length check is not constant-time, but the password this
/// compares against is always exactly 32 hex characters, so a length
/// mismatch reveals nothing an attacker did not already know.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut differs: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        differs |= x ^ y;
    }
    differs == 0
}

// ---------------------------------------------------------------------------
// Dates. No date crate is a dependency here; see the module documentation.
// ---------------------------------------------------------------------------

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// One Unix timestamp, broken into its UTC calendar fields.
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    /// 0 is Sunday, matching `WEEKDAYS`.
    weekday: usize,
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to a
/// proleptic Gregorian year, month, and day. Public domain; this is a
/// direct transcription with the shared 400 year era arithmetic, not a
/// reinvention.
fn civil_from_unix(unix_secs: i64) -> Civil {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let hour = u32::try_from(secs_of_day / 3600).unwrap_or(0);
    let minute = u32::try_from((secs_of_day % 3600) / 60).unwrap_or(0);
    let second = u32::try_from(secs_of_day % 60).unwrap_or(0);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1); // [1, 31]
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1); // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };

    // 1970-01-01 was a Thursday, weekday index 4 with Sunday at 0.
    let weekday = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);

    Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
        weekday,
    }
}

/// `"Tue, 09 Sep 2025 12:00:00 GMT"`, the `getlastmodified` and
/// `Last-Modified` shape, RFC 1123.
pub(crate) fn rfc1123(unix_secs: i64) -> String {
    let c = civil_from_unix(unix_secs);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[c.weekday],
        c.day,
        MONTHS[(c.month - 1) as usize],
        c.year,
        c.hour,
        c.minute,
        c.second
    )
}

/// `"2025-09-09T12:00:00Z"`, the `creationdate` shape, ISO 8601.
///
/// `creationdate` equals the modified time: the peer's file operations
/// layer does not track a separate creation time, so there is nothing else
/// honest to put here.
pub(crate) fn iso8601(unix_secs: i64) -> String {
    let c = civil_from_unix(unix_secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// The inverse of [`rfc1123`]: `"Tue, 09 Sep 2025 12:00:00 GMT"` back to
/// Unix seconds. `None` when `text` is not exactly that shape.
///
/// `docs/engine-contract.md`, item 6, I2: a `PROPPATCH` sets the modified
/// time from `getlastmodified` or `Win32LastModifiedTime`, and both carry
/// this same shape, since a well behaved client only ever echoes back the
/// date this bridge itself wrote with [`rfc1123`].
pub(crate) fn parse_rfc1123(text: &str) -> Option<i64> {
    let (_weekday, rest) = text.trim().split_once(", ")?;
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = u32::try_from(MONTHS.iter().position(|m| *m == month_name)?).ok()? + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Howard Hinnant's `days_from_civil`: the inverse of `civil_from_unix`'s
/// date half. Public domain; see that function's own documentation.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let month = i64::from(month);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe.div_euclid(4) - yoe.div_euclid(100) + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{
        RangeOutcome, base64_decode, constant_time_eq, destination_path, iso8601, parse_range,
        parse_rfc1123, percent_decode, read_head, rfc1123,
    };

    /// Asserts a [`RangeOutcome::Satisfiable`] and returns its span, so a
    /// test reads like the plain tuple the old `Option` API returned.
    fn satisfiable(outcome: &RangeOutcome) -> (u64, u64) {
        match *outcome {
            RangeOutcome::Satisfiable(start, end) => (start, end),
            RangeOutcome::Absent => panic!("expected a satisfiable range, got Absent"),
            RangeOutcome::Unsatisfiable => {
                panic!("expected a satisfiable range, got Unsatisfiable")
            }
        }
    }

    #[test]
    fn base64_decodes_a_basic_auth_pair() {
        assert_eq!(
            base64_decode("ZmVycnk6c2VjcmV0"),
            Some(b"ferry:secret".to_vec())
        );
    }

    #[test]
    fn base64_refuses_a_non_alphabet_byte() {
        assert_eq!(base64_decode("not base64!!"), None);
    }

    #[test]
    fn constant_time_eq_agrees_with_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn rfc1123_matches_known_dates() {
        assert_eq!(rfc1123(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(rfc1123(946_684_800), "Sat, 01 Jan 2000 00:00:00 GMT");
        assert_eq!(rfc1123(1_757_419_200), "Tue, 09 Sep 2025 12:00:00 GMT");
        assert_eq!(rfc1123(1_500_000_000), "Fri, 14 Jul 2017 02:40:00 GMT");
    }

    #[test]
    fn iso8601_matches_known_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(946_684_800), "2000-01-01T00:00:00Z");
        assert_eq!(iso8601(1_757_419_200), "2025-09-09T12:00:00Z");
    }

    #[test]
    fn parse_rfc1123_undoes_rfc1123() {
        for secs in [0, 946_684_800, 1_757_419_200, 1_500_000_000] {
            assert_eq!(
                parse_rfc1123(&rfc1123(secs)),
                Some(secs),
                "round trip at {secs}"
            );
        }
    }

    #[test]
    fn parse_rfc1123_refuses_nonsense() {
        assert_eq!(parse_rfc1123("not a date"), None);
        assert_eq!(parse_rfc1123(""), None);
    }

    #[test]
    fn destination_path_reads_a_matching_host() {
        assert_eq!(
            destination_path(
                Some("http://127.0.0.1:9999/Root/New%20Name.txt"),
                "127.0.0.1:9999"
            ),
            Some("Root/New Name.txt".to_owned())
        );
    }

    #[test]
    fn destination_path_refuses_a_different_host() {
        assert_eq!(
            destination_path(Some("http://example.com/Root/a.txt"), "127.0.0.1:9999"),
            None
        );
    }

    #[test]
    fn destination_path_refuses_a_missing_header() {
        assert_eq!(destination_path(None, "127.0.0.1:9999"), None);
    }

    #[test]
    fn percent_decode_reads_escapes_once() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("Q3%20notes.md"), "Q3 notes.md");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn range_reads_a_bounded_span() {
        assert_eq!(satisfiable(&parse_range("bytes=0-99", 1000)), (0, 99));
        assert_eq!(satisfiable(&parse_range("bytes=900-", 1000)), (900, 999));
        assert_eq!(satisfiable(&parse_range("bytes=-100", 1000)), (900, 999));
    }

    #[test]
    fn range_clamps_an_end_past_the_file() {
        assert_eq!(satisfiable(&parse_range("bytes=0-99999", 1000)), (0, 999));
    }

    #[test]
    fn range_refuses_a_backwards_span() {
        assert!(matches!(
            parse_range("bytes=500-100", 1000),
            RangeOutcome::Unsatisfiable
        ));
    }

    #[test]
    fn range_refuses_a_start_past_the_end() {
        assert!(matches!(
            parse_range("bytes=1000-2000", 1000),
            RangeOutcome::Unsatisfiable
        ));
        // Backwards only once the end is clamped to the last byte: before
        // clamping, 20 <= 30, so this is not caught by the plain
        // start-past-end check on its own.
        assert!(matches!(
            parse_range("bytes=20-30", 10),
            RangeOutcome::Unsatisfiable
        ));
    }

    #[test]
    fn range_refuses_an_empty_suffix() {
        assert!(matches!(
            parse_range("bytes=-0", 1000),
            RangeOutcome::Unsatisfiable
        ));
    }

    #[test]
    fn range_absent_on_a_header_this_side_does_not_understand() {
        assert!(matches!(
            parse_range("not-bytes", 1000),
            RangeOutcome::Absent
        ));
        assert!(matches!(
            parse_range("bytes=abc-99", 1000),
            RangeOutcome::Absent
        ));
    }

    /// B3: `server.rs::accept_loop` sets a read timeout on every accepted
    /// stream, so a connection that sends nothing is closed rather than
    /// held forever. This proves the half `read_head` is responsible for:
    /// once a read times out, it gives up and returns promptly instead of
    /// blocking again or looping, using a short timeout this test sets
    /// itself rather than waiting out the real 30 second one.
    #[test]
    fn read_head_gives_up_once_the_socket_times_out() {
        use std::io::BufReader;
        use std::net::{TcpListener, TcpStream};
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let addr = listener.local_addr().expect("a bound address");
        // Held for the life of the test and never written to: the silent
        // peer `read_head` is refusing to wait forever on.
        let _silent_peer = TcpStream::connect(addr).expect("a silent connection");

        let (accepted, _) = listener.accept().expect("the silent connection to accept");
        accepted
            .set_read_timeout(Some(Duration::from_millis(150)))
            .expect("a short read timeout should set");
        let mut reader = BufReader::new(accepted);

        let started = Instant::now();
        let result = read_head(&mut reader);
        assert!(
            result.is_err(),
            "a read that times out must surface as an error, not a request"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?} to give up on a silent connection",
            started.elapsed()
        );
    }
}
