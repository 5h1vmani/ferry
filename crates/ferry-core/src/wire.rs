//! Encoding and decoding of the values the protocol sends.
//!
//! The format is written by hand rather than taken from a serialisation crate.
//! The wire protocol is meant to be implementable from `docs/protocol.md`
//! alone, so the document has to describe every byte. A hand-written codec
//! keeps the document and the code honest about each other.
//!
//! All integers are big endian. Byte strings and text carry a `u32` length
//! first.

use std::fmt;

/// The reason a value could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The input ended in the middle of a value.
    UnexpectedEnd,
    /// A length field was larger than the limit for its kind.
    TooLong,
    /// Text was not valid UTF-8.
    NotUtf8,
    /// A tag byte did not name anything this version knows.
    UnknownTag(u8),
    /// Bytes were left over after the value was decoded.
    TrailingBytes,
    /// A path was not valid, for a reason other than its length.
    ///
    /// See `crate::path::RemotePath::parse`, which runs the checks.
    InvalidPath,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("input ended in the middle of a value"),
            Self::TooLong => f.write_str("a length field was over its limit"),
            Self::NotUtf8 => f.write_str("text was not valid UTF-8"),
            Self::UnknownTag(t) => write!(f, "unknown tag byte {t}"),
            Self::TrailingBytes => f.write_str("bytes were left over after decoding"),
            Self::InvalidPath => f.write_str("path failed validation"),
        }
    }
}

impl std::error::Error for WireError {}

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

#[cfg(test)]
mod tests {
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
}
