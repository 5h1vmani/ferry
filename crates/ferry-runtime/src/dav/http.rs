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
use std::io::{self, BufRead, Write};

use crate::state::now_unix_secs;

/// One parsed request line, its headers, and its body.
///
/// Header names are lowercased on the way in, so a caller never has to
/// guess a peer's capitalisation.
pub(crate) struct Request {
    pub(crate) method: String,
    /// The request target exactly as sent, still percent-encoded. The
    /// caller decodes it once, per `docs/engine-contract.md`, item 6.
    pub(crate) target: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Reads one request. Returns `Ok(None)` when the connection closed before
/// a request line arrived, which is the ordinary end of a connection
/// between requests, not a fault.
pub(crate) fn read_request(reader: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let request_line = request_line.trim_end();
    if request_line.is_empty() {
        return Ok(None);
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("/").to_owned();

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            // A connection that closes mid-headers has sent no request
            // this side can answer.
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    let body_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; body_len];
    if body_len > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        target,
        headers,
        body,
    }))
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

/// Splits a `Range: bytes=...` value into an inclusive `(start, end)`,
/// against a file of `total` bytes. `None` on anything this side does not
/// understand, which the caller treats as "no range".
pub(crate) fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if a.trim().is_empty() {
        let suffix: u64 = b.trim().parse().ok()?;
        return Some((total.saturating_sub(suffix), total.saturating_sub(1)));
    }
    let start: u64 = a.trim().parse().ok()?;
    let end = if b.trim().is_empty() {
        total.saturating_sub(1)
    } else {
        b.trim().parse().ok()?
    };
    if start > end {
        return None;
    }
    Some((start, end.min(total.saturating_sub(1))))
}

/// `"<size>-<mtime>"`, quoted, per `docs/engine-contract.md`, item 6.
pub(crate) fn etag(size: u64, modified_unix_secs: i64) -> String {
    format!("\"{size}-{modified_unix_secs}\"")
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

#[cfg(test)]
mod tests {
    use super::{base64_decode, constant_time_eq, iso8601, parse_range, percent_decode, rfc1123};

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
    fn percent_decode_reads_escapes_once() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("Q3%20notes.md"), "Q3 notes.md");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn range_reads_a_bounded_span() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
    }

    #[test]
    fn range_clamps_an_end_past_the_file() {
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
    }

    #[test]
    fn range_refuses_a_backwards_span() {
        assert_eq!(parse_range("bytes=500-100", 1000), None);
    }
}
