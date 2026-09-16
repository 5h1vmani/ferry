//! Encoding and decoding of the values the protocol sends.
//!
//! The format is written by hand rather than taken from a serialisation crate.
//! The wire protocol is meant to be implementable from `docs/protocol.md`
//! alone, so the document has to describe every byte. A hand-written codec
//! keeps the document and the code honest about each other.
//!
//! All integers are big endian. Byte strings and text carry a `u32` length
//! first.

/// The reason a value could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The input ended in the middle of a value.
    #[error("input ended in the middle of a value")]
    UnexpectedEnd,
    /// A length field was larger than the limit for its kind.
    #[error("a length field was over its limit")]
    TooLong,
    /// Text was not valid UTF-8.
    #[error("text was not valid UTF-8")]
    NotUtf8,
    /// A tag byte did not name anything this version knows.
    #[error("unknown tag byte {0}")]
    UnknownTag(u8),
    /// Bytes were left over after the value was decoded.
    #[error("bytes were left over after decoding")]
    TrailingBytes,
    /// A path was not valid, for a reason other than its length.
    ///
    /// See `crate::path::RemotePath::parse`, which runs the checks.
    #[error("path failed validation")]
    InvalidPath,
    /// A manifest was not valid.
    ///
    /// See `crate::chunk::Manifest::decode`, which runs the checks: a bad
    /// chunk size, a chunk count that disagrees with the length, or chunks
    /// that do not merge to the stated root hash.
    #[error("manifest failed validation")]
    BadManifest,
}

/// Appends values to a growing buffer.
#[derive(Debug, Default)]
pub struct Encoder {
    out: Vec<u8>,
}

impl Encoder {
    /// Start an empty encoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start an encoder with room for at least `bytes` bytes already set
    /// aside.
    ///
    /// Use this when the final size is known ahead of time. An encoder
    /// started with `new` grows its buffer as values are appended, and each
    /// growth step copies the old bytes into a new, larger allocation and
    /// frees the old one. For most data that copy is harmless. For secret
    /// data, such as a private key, the freed allocation still holds a copy
    /// of the secret and is never cleared. Reserving the right capacity up
    /// front means the buffer is allocated once and never moves.
    #[must_use]
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            out: Vec::with_capacity(bytes),
        }
    }

    /// Append one byte.
    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.out.push(value);
        self
    }

    /// Append a big endian `u16`.
    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.out.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Append a big endian `u32`.
    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.out.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Append a big endian `u64`.
    pub fn u64(&mut self, value: u64) -> &mut Self {
        self.out.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// Append a length-prefixed byte string.
    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
        self.u32(len);
        self.out.extend_from_slice(value);
        self
    }

    /// Append length-prefixed UTF-8 text.
    pub fn text(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    /// Append a fixed-size array without a length prefix.
    pub fn fixed<const N: usize>(&mut self, value: &[u8; N]) -> &mut Self {
        self.out.extend_from_slice(value);
        self
    }

    /// Take the encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.out
    }

    /// Look at the encoded bytes so far.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.out
    }
}

/// Reads values out of a byte slice, in order.
#[derive(Debug)]
pub struct Decoder<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Decoder<'a> {
    /// Start reading `input` from the beginning.
    #[must_use]
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, at: 0 }
    }

    /// How many bytes are left.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.input.len() - self.at
    }

    /// Fail unless every byte has been read.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TrailingBytes`] when bytes are left over. Extra
    /// bytes usually mean the two sides disagree about the format, so the
    /// message is rejected rather than half understood.
    pub fn finish(self) -> Result<(), WireError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(WireError::TrailingBytes)
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.remaining() < n {
            return Err(WireError::UnexpectedEnd);
        }
        let slice = &self.input[self.at..self.at + n];
        self.at += n;
        Ok(slice)
    }

    /// Read one byte.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when the input is exhausted.
    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    /// Read a big endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when fewer than two bytes remain.
    pub fn u16(&mut self) -> Result<u16, WireError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Read a big endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when fewer than four bytes remain.
    pub fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a big endian `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when fewer than eight bytes remain.
    pub fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a length-prefixed byte string, refusing anything over `max`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TooLong`] when the declared length is over `max`,
    /// and [`WireError::UnexpectedEnd`] when the bytes are not all there. The
    /// limit is checked before any memory is reserved, so a large length field
    /// cannot exhaust memory on its own.
    pub fn bytes(&mut self, max: usize) -> Result<&'a [u8], WireError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(WireError::TooLong);
        }
        self.take(len)
    }

    /// Read length-prefixed UTF-8 text, refusing anything over `max` bytes.
    ///
    /// # Errors
    ///
    /// As [`Decoder::bytes`], plus [`WireError::NotUtf8`] for invalid text.
    pub fn text(&mut self, max: usize) -> Result<&'a str, WireError> {
        let raw = self.bytes(max)?;
        core::str::from_utf8(raw).map_err(|_| WireError::NotUtf8)
    }

    /// Read a fixed-size array.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when fewer than `N` bytes remain.
    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let slice = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }
}

/// Turn a signed Unix second count into the `u64` [`Encoder::u64`] carries.
///
/// The wire format has no signed integer, so a timestamp that can be before
/// 1970, such as a peer's `paired_unix_secs`, is bit-cast into a `u64`
/// instead of being clamped to zero. [`decode_i64`] reverses this exactly,
/// with no loss, because both sides are the same width.
#[must_use]
pub fn encode_i64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

/// The inverse of [`encode_i64`].
#[must_use]
pub fn decode_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{Decoder, Encoder, WireError};

    #[test]
    fn every_primitive_survives_a_round_trip() {
        let mut e = Encoder::new();
        e.u8(0xAB)
            .u16(0x1234)
            .u32(0xDEAD_BEEF)
            .u64(0x0102_0304_0506_0708)
            .bytes(b"raw")
            .text("DCIM/Camera")
            .fixed(&[9u8; 32]);
        let encoded = e.finish();

        let mut d = Decoder::new(&encoded);
        assert_eq!(d.u8().unwrap(), 0xAB);
        assert_eq!(d.u16().unwrap(), 0x1234);
        assert_eq!(d.u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(d.u64().unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(d.bytes(16).unwrap(), b"raw");
        assert_eq!(d.text(64).unwrap(), "DCIM/Camera");
        assert_eq!(d.fixed::<32>().unwrap(), [9u8; 32]);
        d.finish().unwrap();
    }

    #[test]
    fn integers_are_big_endian() {
        let mut e = Encoder::new();
        e.u32(1);
        assert_eq!(e.finish(), vec![0, 0, 0, 1]);
    }

    #[test]
    fn a_truncated_value_is_rejected() {
        let encoded = Encoder::new().u32(7).as_slice()[..3].to_vec();
        assert_eq!(Decoder::new(&encoded).u32(), Err(WireError::UnexpectedEnd));
    }

    #[test]
    fn a_length_over_the_limit_is_rejected_before_reading() {
        // The length claims four gibibytes. Nothing is allocated.
        let mut e = Encoder::new();
        e.u32(u32::MAX);
        let encoded = e.finish();
        assert_eq!(Decoder::new(&encoded).bytes(1024), Err(WireError::TooLong));
    }

    #[test]
    fn a_length_that_outruns_the_input_is_rejected() {
        let mut e = Encoder::new();
        e.u32(100);
        e.fixed(&[1u8; 4]);
        let encoded = e.finish();
        assert_eq!(
            Decoder::new(&encoded).bytes(1024),
            Err(WireError::UnexpectedEnd)
        );
    }

    #[test]
    fn invalid_text_is_rejected() {
        let mut e = Encoder::new();
        e.bytes(&[0xFF, 0xFE]);
        let encoded = e.finish();
        assert_eq!(Decoder::new(&encoded).text(64), Err(WireError::NotUtf8));
    }

    #[test]
    fn leftover_bytes_are_rejected() {
        let mut e = Encoder::new();
        e.u8(1).u8(2);
        let encoded = e.finish();
        let mut d = Decoder::new(&encoded);
        assert_eq!(d.u8().unwrap(), 1);
        assert_eq!(d.finish(), Err(WireError::TrailingBytes));
    }

    // Property tests below. The fixed tests above cover the documented
    // behaviour; these check it holds over generated values too.

    /// Text built from arbitrary Unicode characters, capped by character
    /// count. Four bytes per character is the worst case, so the encoded
    /// form never grows past four times `max_chars` bytes.
    fn arbitrary_text(max_chars: usize) -> impl Strategy<Value = String> {
        proptest::collection::vec(any::<char>(), 0..max_chars)
            .prop_map(|chars| chars.into_iter().collect())
    }

    /// A byte string plus a truncation point that always removes at least
    /// one byte from its encoded form, so decoding it can only fail.
    fn bytes_and_a_truncation_point() -> impl Strategy<Value = (Vec<u8>, usize)> {
        proptest::collection::vec(any::<u8>(), 0..256).prop_flat_map(|payload| {
            let full_len = 4 + payload.len();
            (Just(payload), 0..full_len)
        })
    }

    /// The text version of `bytes_and_a_truncation_point`.
    fn text_and_a_truncation_point() -> impl Strategy<Value = (String, usize)> {
        arbitrary_text(64).prop_flat_map(|text| {
            let full_len = 4 + text.len();
            (Just(text), 0..full_len)
        })
    }

    /// One value from the small set the mixed-sequence property below
    /// encodes. A single generated list can then mix several wire types in
    /// one buffer, in a chosen order.
    #[derive(Debug)]
    enum TaggedValue {
        U8(u8),
        U16(u16),
        U32(u32),
        Bytes(Vec<u8>),
        Text(String),
    }

    fn tagged_value() -> impl Strategy<Value = TaggedValue> {
        prop_oneof![
            any::<u8>().prop_map(TaggedValue::U8),
            any::<u16>().prop_map(TaggedValue::U16),
            any::<u32>().prop_map(TaggedValue::U32),
            proptest::collection::vec(any::<u8>(), 0..256).prop_map(TaggedValue::Bytes),
            arbitrary_text(64).prop_map(TaggedValue::Text),
        ]
    }

    proptest! {
        #[test]
        fn every_primitive_survives_a_generated_round_trip(
            byte in any::<u8>(),
            short in any::<u16>(),
            word in any::<u32>(),
            quad in any::<u64>(),
            blob in proptest::collection::vec(any::<u8>(), 0..4096),
            text in arbitrary_text(256),
        ) {
            let encoded = { let mut e = Encoder::new(); e.u8(byte); e.finish() };
            let mut d = Decoder::new(&encoded);
            prop_assert_eq!(d.u8().unwrap(), byte);
            prop_assert!(d.finish().is_ok());

            let encoded = { let mut e = Encoder::new(); e.u16(short); e.finish() };
            let mut d = Decoder::new(&encoded);
            prop_assert_eq!(d.u16().unwrap(), short);
            prop_assert!(d.finish().is_ok());

            let encoded = { let mut e = Encoder::new(); e.u32(word); e.finish() };
            let mut d = Decoder::new(&encoded);
            prop_assert_eq!(d.u32().unwrap(), word);
            prop_assert!(d.finish().is_ok());

            let encoded = { let mut e = Encoder::new(); e.u64(quad); e.finish() };
            let mut d = Decoder::new(&encoded);
            prop_assert_eq!(d.u64().unwrap(), quad);
            prop_assert!(d.finish().is_ok());

            let encoded = { let mut e = Encoder::new(); e.bytes(&blob); e.finish() };
            let mut d = Decoder::new(&encoded);
            prop_assert_eq!(d.bytes(4096).unwrap(), blob.as_slice());
            prop_assert!(d.finish().is_ok());

            let encoded = { let mut e = Encoder::new(); e.text(&text); e.finish() };
            let mut d = Decoder::new(&encoded);
            prop_assert_eq!(d.text(1024).unwrap(), text.as_str());
            prop_assert!(d.finish().is_ok());
        }

        #[test]
        fn a_mixed_sequence_of_values_round_trips_in_order(
            values in proptest::collection::vec(tagged_value(), 0..16)
        ) {
            let mut e = Encoder::new();
            for value in &values {
                match value {
                    TaggedValue::U8(v) => { e.u8(0).u8(*v); }
                    TaggedValue::U16(v) => { e.u8(1).u16(*v); }
                    TaggedValue::U32(v) => { e.u8(2).u32(*v); }
                    TaggedValue::Bytes(v) => { e.u8(3).bytes(v); }
                    TaggedValue::Text(v) => { e.u8(4).text(v); }
                }
            }
            let encoded = e.finish();

            let mut d = Decoder::new(&encoded);
            for value in &values {
                let tag = d.u8().unwrap();
                match (tag, value) {
                    (0, TaggedValue::U8(expected)) => prop_assert_eq!(d.u8().unwrap(), *expected),
                    (1, TaggedValue::U16(expected)) => prop_assert_eq!(d.u16().unwrap(), *expected),
                    (2, TaggedValue::U32(expected)) => prop_assert_eq!(d.u32().unwrap(), *expected),
                    (3, TaggedValue::Bytes(expected)) => {
                        prop_assert_eq!(d.bytes(4096).unwrap(), expected.as_slice());
                    }
                    (4, TaggedValue::Text(expected)) => {
                        prop_assert_eq!(d.text(1024).unwrap(), expected.as_str());
                    }
                    _ => prop_assert!(false, "the tag byte did not match the value that wrote it"),
                }
            }
            prop_assert!(d.finish().is_ok());
        }

        #[test]
        fn truncating_an_encoded_u32_is_always_rejected(value in any::<u32>(), cut in 0..4_usize) {
            // Four bytes are always written for a u32. Cutting to fewer than
            // four must fail instead of reading past the end.
            let encoded = { let mut e = Encoder::new(); e.u32(value); e.finish() };
            prop_assert_eq!(Decoder::new(&encoded[..cut]).u32(), Err(WireError::UnexpectedEnd));
        }

        #[test]
        fn truncating_an_encoded_byte_string_is_always_rejected(
            (payload, cut) in bytes_and_a_truncation_point()
        ) {
            let encoded = { let mut e = Encoder::new(); e.bytes(&payload); e.finish() };
            prop_assert_eq!(
                Decoder::new(&encoded[..cut]).bytes(4096),
                Err(WireError::UnexpectedEnd)
            );
        }

        #[test]
        fn truncating_an_encoded_text_value_is_always_rejected(
            (text, cut) in text_and_a_truncation_point()
        ) {
            let encoded = { let mut e = Encoder::new(); e.text(&text); e.finish() };
            prop_assert_eq!(
                Decoder::new(&encoded[..cut]).text(1024),
                Err(WireError::UnexpectedEnd)
            );
        }

        #[test]
        fn a_byte_string_longer_than_the_limit_is_always_too_long(
            max in 0usize..512,
            extra in 1usize..=512,
        ) {
            // The limit is checked before any bytes are read, so it must fire
            // for every length over it, not only for one hand-picked case.
            let payload = vec![0u8; max + extra];
            let encoded = { let mut e = Encoder::new(); e.bytes(&payload); e.finish() };
            prop_assert_eq!(Decoder::new(&encoded).bytes(max), Err(WireError::TooLong));
        }
    }
}
