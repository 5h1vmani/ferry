//! The QR pairing offer: what the Mac draws as a QR code, and what the phone
//! decodes after it scans it.
//!
//! `docs/engine-contract.md` item 12. The payload is ASCII text, so it draws
//! cleanly as a QR code and survives being copied as a string:
//!
//! ```text
//! "FERRY1:" then base64url of:
//!   version(1)
//!   static key(32)
//!   expiry(8)          -- Unix seconds, big endian, signed
//!   nonce(16)
//!   address count(1)
//!   for each address: tag(1, 4 or 16) then that many bytes of IP then port(2)
//! ```
//!
//! The base64url here is hand written, with no padding and no dependency:
//! this is the only place in the crate that needs it, and the alphabet and
//! the padding rule are both small enough to write once and test directly.
//!
//! This module only encodes and decodes the payload. Checking whether an
//! offer has expired needs "now", and checking whether a key is already
//! paired needs the peer store, and neither belongs to a format module, so
//! both stay out: [`Offer::is_expired`] takes "now" as a plain argument
//! instead of reading a clock, and `AlreadyPaired` is raised by
//! `ferry-runtime`, which holds the peer store.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::noise::PublicKey;

/// Marks the payload as a Ferry pairing offer, format version 1.
const OFFER_PREFIX: &str = "FERRY1:";

/// How many bytes the fixed part of the payload is, before the address list:
/// version(1) + static key(32) + expiry(8) + nonce(16) + address count(1).
const HEADER_LEN: usize = 1 + 32 + 8 + 16 + 1;

/// A QR pairing offer, decoded from or ready to encode as the bytes drawn in
/// the QR code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The offer format version. Informational: this build only ever writes
    /// and reads [`OFFER_PREFIX`]'s own version, 1.
    pub version: u8,
    /// The Mac's static public key. The phone dials it directly, over `IK`,
    /// with no separate discovery step.
    pub static_key: PublicKey,
    /// When this offer stops being valid, in Unix seconds.
    pub expires_unix_secs: i64,
    /// Single use. Proves this exact QR code, not a screenshot of an older
    /// one, was scanned.
    pub nonce: [u8; crate::noise::QR_NONCE_LEN],
    /// Where to dial, tried in order. Wi-Fi addresses only: over the cable
    /// the phone cannot reach the Mac.
    pub addresses: Vec<SocketAddr>,
}

/// The reason a scanned offer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// The offer's `expires_unix_secs` is not in the future any more.
    #[error("the pairing offer has expired")]
    OfferExpired,
    /// The scanned text is not a Ferry pairing offer: the wrong prefix, bad
    /// base64url, a truncated or oversized payload, an address tag that is
    /// neither 4 nor 16, or bytes left over after the last address.
    #[error("the scanned code is not a Ferry pairing offer")]
    OfferNotFerry,
    /// This device already holds a stored key for the offer's static key.
    #[error("this device is already paired with that key")]
    AlreadyPaired,
    /// The app's camera permission was refused.
    ///
    /// Never raised by this crate, or anywhere else in `ferry-core` or
    /// `ferry-runtime`: the camera is the app's own concern, on whichever
    /// platform has one. This variant exists so the app can report a
    /// refused permission through the same error type as every other
    /// pairing refusal, with a row in `design/errors.json` like all the
    /// others.
    #[error("the camera was refused")]
    CameraRefused,
}

/// The most addresses a dial ever tries, whichever side is building or
/// reading the list: the offering side's own interfaces
/// (`ferry-runtime`'s `local_wifi_addresses`), and a scanned offer's own
/// list before `dial_offer` tries any of them. A machine, or a hostile
/// offer, naming more than this would otherwise make the dial loop that
/// follows try that many addresses before giving up.
pub const MAX_DIAL_ADDRESSES: usize = 8;

/// True for an address that is only meaningful together with the
/// interface it came from: `169.254.0.0/16`, or `fe80::/10`.
///
/// `Ipv4Addr::is_link_local` already exists in `std`. Its `Ipv6Addr`
/// counterpart is not yet stable, so the top ten bits are checked by
/// hand: `0xfe80` masked with `0xffc0` is exactly `fe80::/10`.
fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
    }
}

/// True for a private-range address: RFC 1918 (`10.0.0.0/8`,
/// `172.16.0.0/12`, `192.168.0.0/16`) or a unique local address
/// (`fc00::/7`). The address a phone on the same Wi-Fi network can
/// actually reach is almost always one of these.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xfe00 == 0xfc00,
    }
}

/// Filters and orders a list of dial addresses to one policy, shared by
/// the offering side (its own local interfaces) and the scanning side (a
/// received offer's own list, which a hostile peer controls): loopback
/// and link-local addresses are dropped outright, since neither is ever
/// meaningful across this offer; a private-range address sorts before a
/// public one, since it is the address a phone on the same Wi-Fi network
/// can actually reach; and at most [`MAX_DIAL_ADDRESSES`] survive, kept in
/// their original relative order within each group, so a peer naming
/// dozens of addresses cannot make a dial loop try more than a handful.
#[must_use]
pub fn dialable_addresses(addresses: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let mut kept: Vec<SocketAddr> = addresses
        .into_iter()
        .filter(|addr| !addr.ip().is_loopback() && !is_link_local(addr.ip()))
        .collect();
    kept.sort_by_key(|addr| !is_private(addr.ip()));
    kept.truncate(MAX_DIAL_ADDRESSES);
    kept
}

impl Offer {
    /// Encode this offer as the bytes a QR code draws.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut inner = Vec::with_capacity(HEADER_LEN + self.addresses.len() * 19);
        inner.push(self.version);
        inner.extend_from_slice(self.static_key.as_bytes());
        inner.extend_from_slice(&self.expires_unix_secs.to_be_bytes());
        inner.extend_from_slice(&self.nonce);
        // Address counts beyond 255 cannot happen in practice: this build
        // offers at most a handful of local interface addresses. Saturating
        // rather than panicking keeps `encode` infallible.
        inner.push(u8::try_from(self.addresses.len()).unwrap_or(u8::MAX));
        for addr in self.addresses.iter().take(usize::from(u8::MAX)) {
            match addr.ip() {
                IpAddr::V4(v4) => {
                    inner.push(4);
                    inner.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    inner.push(16);
                    inner.extend_from_slice(&v6.octets());
                }
            }
            inner.extend_from_slice(&addr.port().to_be_bytes());
        }

        let mut out = Vec::with_capacity(OFFER_PREFIX.len() + inner.len().div_ceil(3) * 4);
        out.extend_from_slice(OFFER_PREFIX.as_bytes());
        out.extend_from_slice(encode_base64url(&inner).as_bytes());
        out
    }

    /// Decode what [`Offer::encode`] built.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::OfferNotFerry`] for anything that is not a
    /// well formed offer of this format: the wrong prefix, invalid
    /// base64url, a truncated payload, an address tag that is neither 4 nor
    /// 16, or trailing bytes after the last address.
    pub fn decode(payload: &[u8]) -> Result<Self, PairingError> {
        let text = std::str::from_utf8(payload).map_err(|_| PairingError::OfferNotFerry)?;
        let encoded = text
            .strip_prefix(OFFER_PREFIX)
            .ok_or(PairingError::OfferNotFerry)?;
        let inner = decode_base64url(encoded).ok_or(PairingError::OfferNotFerry)?;

        if inner.len() < HEADER_LEN {
            return Err(PairingError::OfferNotFerry);
        }
        let version = inner[0];
        if version != 1 {
            return Err(PairingError::OfferNotFerry);
        }
        let mut static_key = [0u8; 32];
        static_key.copy_from_slice(&inner[1..33]);
        let mut expiry_bytes = [0u8; 8];
        expiry_bytes.copy_from_slice(&inner[33..41]);
        let expires_unix_secs = i64::from_be_bytes(expiry_bytes);
        let mut nonce = [0u8; crate::noise::QR_NONCE_LEN];
        nonce.copy_from_slice(&inner[41..57]);
        let count = inner[57];

        let mut addresses = Vec::with_capacity(usize::from(count));
        let mut pos = HEADER_LEN;
        for _ in 0..count {
            let tag = *inner.get(pos).ok_or(PairingError::OfferNotFerry)?;
            pos += 1;
            let ip = match tag {
                4 => {
                    let bytes = inner.get(pos..pos + 4).ok_or(PairingError::OfferNotFerry)?;
                    pos += 4;
                    IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]))
                }
                16 => {
                    let bytes = inner
                        .get(pos..pos + 16)
                        .ok_or(PairingError::OfferNotFerry)?;
                    pos += 16;
                    let mut octets = [0u8; 16];
                    octets.copy_from_slice(bytes);
                    IpAddr::V6(Ipv6Addr::from(octets))
                }
                _ => return Err(PairingError::OfferNotFerry),
            };
            let port_bytes = inner.get(pos..pos + 2).ok_or(PairingError::OfferNotFerry)?;
            pos += 2;
            addresses.push(SocketAddr::new(
                ip,
                u16::from_be_bytes([port_bytes[0], port_bytes[1]]),
            ));
        }
        if pos != inner.len() {
            return Err(PairingError::OfferNotFerry);
        }

        Ok(Self {
            version,
            static_key: PublicKey(static_key),
            expires_unix_secs,
            nonce,
            addresses,
        })
    }

    /// Whether this offer is no longer valid at `now_unix_secs`.
    ///
    /// Takes the current time as an argument, rather than reading a clock
    /// itself, so a test can check both sides of the boundary without
    /// sleeping and so `ferry-runtime`'s own `now_unix_secs` stays the one
    /// place wall-clock time is read.
    #[must_use]
    pub fn is_expired(&self, now_unix_secs: i64) -> bool {
        now_unix_secs >= self.expires_unix_secs
    }
}

/// The base64url alphabet, RFC 4648 section 5. No padding is produced.
const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(char::from(
            BASE64URL_ALPHABET[usize::try_from((n >> 18) & 0x3f).unwrap()],
        ));
        out.push(char::from(
            BASE64URL_ALPHABET[usize::try_from((n >> 12) & 0x3f).unwrap()],
        ));
        if chunk.len() > 1 {
            out.push(char::from(
                BASE64URL_ALPHABET[usize::try_from((n >> 6) & 0x3f).unwrap()],
            ));
        }
        if chunk.len() > 2 {
            out.push(char::from(
                BASE64URL_ALPHABET[usize::try_from(n & 0x3f).unwrap()],
            ));
        }
    }
    out
}

/// The value of one base64url character, or `None` for anything outside the
/// alphabet, padding included: this codec never produces padding, so a `=`
/// in decoded text is treated the same as any other invalid character.
fn base64url_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

fn decode_base64url(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() {
        return None;
    }
    let bytes = text.as_bytes();
    // A group of one leftover character cannot represent a whole byte.
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    for chunk in bytes.chunks(4) {
        let values: Vec<u8> = chunk
            .iter()
            .map(|&b| base64url_value(b))
            .collect::<Option<_>>()?;
        let n = values
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &v)| acc | (u32::from(v) << (18 - 6 * i)));
        out.push(u8::try_from((n >> 16) & 0xff).unwrap());
        if values.len() > 2 {
            out.push(u8::try_from((n >> 8) & 0xff).unwrap());
        }
        if values.len() > 3 {
            out.push(u8::try_from(n & 0xff).unwrap());
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_DIAL_ADDRESSES, Offer, PairingError, decode_base64url, dialable_addresses,
        encode_base64url,
    };
    use crate::noise::{PublicKey, QR_NONCE_LEN};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), port)
    }

    #[test]
    fn dialable_addresses_drops_loopback_and_link_local() {
        let addresses = vec![
            v4(127, 0, 0, 1, 1),
            v4(169, 254, 1, 2, 1),
            v4(192, 168, 1, 42, 1),
        ];
        assert_eq!(dialable_addresses(addresses), vec![v4(192, 168, 1, 42, 1)]);
    }

    #[test]
    fn dialable_addresses_puts_private_range_addresses_first() {
        let public = v4(203, 0, 113, 5, 1);
        let private = v4(10, 0, 0, 7, 1);
        assert_eq!(
            dialable_addresses(vec![public, private]),
            vec![private, public],
            "a private-range address is the one a phone on the same Wi-Fi can reach"
        );
    }

    #[test]
    fn dialable_addresses_caps_at_the_maximum_even_from_twenty() {
        let addresses: Vec<SocketAddr> = (0..20).map(|i| v4(10, 0, 0, i, 1)).collect();
        let kept = dialable_addresses(addresses);
        assert_eq!(
            kept.len(),
            MAX_DIAL_ADDRESSES,
            "an offer with 20 addresses dials at most {MAX_DIAL_ADDRESSES}"
        );
    }

    #[test]
    fn dialable_addresses_keeps_relative_order_within_each_group() {
        let addresses = vec![
            v4(10, 0, 0, 1, 1),
            v4(203, 0, 113, 9, 1),
            v4(10, 0, 0, 2, 1),
            v4(203, 0, 113, 8, 1),
        ];
        assert_eq!(
            dialable_addresses(addresses),
            vec![
                v4(10, 0, 0, 1, 1),
                v4(10, 0, 0, 2, 1),
                v4(203, 0, 113, 9, 1),
                v4(203, 0, 113, 8, 1),
            ]
        );
    }

    fn sample_offer() -> Offer {
        Offer {
            version: 1,
            static_key: PublicKey([9u8; 32]),
            expires_unix_secs: 1_800_000_000,
            nonce: [5u8; QR_NONCE_LEN],
            addresses: vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42)), 54321),
                SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
                    54321,
                ),
            ],
        }
    }

    #[test]
    fn base64url_round_trips_every_tail_length() {
        for len in 0..=9 {
            let bytes: Vec<u8> = (0..len)
                .map(|i| u8::try_from(i * 7 % 251).unwrap())
                .collect();
            let encoded = encode_base64url(&bytes);
            assert!(!encoded.contains('='), "no padding for length {len}");
            assert_eq!(decode_base64url(&encoded).unwrap(), bytes, "length {len}");
        }
    }

    #[test]
    fn an_offer_round_trips_through_encode_and_decode() {
        let offer = sample_offer();
        let payload = offer.encode();
        assert!(
            payload.starts_with(b"FERRY1:"),
            "a QR offer must start with the format tag"
        );
        assert!(
            payload.is_ascii(),
            "the payload must be plain ASCII, safe to draw as a QR code"
        );
        assert_eq!(Offer::decode(&payload).unwrap(), offer);
    }

    #[test]
    fn an_offer_with_no_addresses_still_round_trips() {
        let mut offer = sample_offer();
        offer.addresses.clear();
        let payload = offer.encode();
        assert_eq!(Offer::decode(&payload).unwrap(), offer);
    }

    #[test]
    fn a_version_other_than_one_is_refused() {
        let mut offer = sample_offer();
        offer.version = 2;
        let payload = offer.encode();
        match Offer::decode(&payload) {
            Err(PairingError::OfferNotFerry) => {}
            other => panic!("expected OfferNotFerry, got {other:?}"),
        }
    }

    #[test]
    fn the_wrong_prefix_is_refused() {
        match Offer::decode(b"NOTFERRY:abc") {
            Err(PairingError::OfferNotFerry) => {}
            other => panic!("expected OfferNotFerry, got {other:?}"),
        }
    }

    #[test]
    fn invalid_base64_is_refused() {
        match Offer::decode(b"FERRY1:not*valid*base64") {
            Err(PairingError::OfferNotFerry) => {}
            other => panic!("expected OfferNotFerry, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_payload_is_refused() {
        let full = sample_offer().encode();
        // Cut it well before the address list even starts.
        let short = &full[..full.len() - 40];
        match Offer::decode(short) {
            Err(PairingError::OfferNotFerry) => {}
            other => panic!("expected OfferNotFerry, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_address_tag_is_refused() {
        // `Offer::encode` only ever writes tag 4 or 16, so a bad tag has to
        // be hand built: the same header fields, then one address entry
        // whose tag is neither.
        let mut inner = vec![1u8]; // version
        inner.extend_from_slice(&[0u8; 32]); // static key
        inner.extend_from_slice(&0i64.to_be_bytes()); // expiry
        inner.extend_from_slice(&[0u8; QR_NONCE_LEN]); // nonce
        inner.push(1); // one address
        inner.push(6); // neither 4 nor 16
        inner.extend_from_slice(&[0u8; 6]);
        inner.extend_from_slice(&0u16.to_be_bytes());
        let mut payload = Vec::new();
        payload.extend_from_slice(b"FERRY1:");
        payload.extend_from_slice(encode_base64url(&inner).as_bytes());
        match Offer::decode(&payload) {
            Err(PairingError::OfferNotFerry) => {}
            other => panic!("expected OfferNotFerry, got {other:?}"),
        }
    }

    #[test]
    fn trailing_bytes_after_the_last_address_are_refused() {
        let offer = sample_offer();
        let mut payload = offer.encode();
        // Append one more base64url character's worth of junk after a
        // well-formed payload, so decoding leaves bytes unconsumed.
        payload.push(b'A');
        payload.push(b'A');
        payload.push(b'A');
        payload.push(b'A');
        match Offer::decode(&payload) {
            Err(PairingError::OfferNotFerry) => {}
            other => panic!("expected OfferNotFerry, got {other:?}"),
        }
    }

    #[test]
    fn expiry_is_checked_against_the_given_time_not_a_clock() {
        let offer = sample_offer();
        assert!(!offer.is_expired(offer.expires_unix_secs - 1));
        assert!(offer.is_expired(offer.expires_unix_secs));
        assert!(offer.is_expired(offer.expires_unix_secs + 1));
    }
}
