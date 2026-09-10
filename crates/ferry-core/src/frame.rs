//! The frame codec.
//!
//! A frame is the unit the protocol exchanges once a connection is encrypted.
//! Every frame looks the same:
//!
//! ```text
//! [u32 payload_len][u8 kind][u32 request_id][payload]
//! ```
//!
//! The length comes first and is checked against [`limits::MAX_FRAME_PAYLOAD`]
//! before any memory is reserved. An unbounded length would let a peer exhaust
//! memory with four bytes.
//!
//! The request identifier lets several requests be in flight at once. Answers
//! may come back in any order, which is what keeps many small files fast.
//!
//! This codec runs over a plain byte stream. When the stream is encrypted, the
//! Noise layer below splits those bytes into transport messages. Framing does
//! not know about that split, and does not need to.

use std::io::{self, Read, Write};

use crate::limits;
use crate::wire::WireError;

/// What a frame is for.
///
/// A `Hello` frame always carries a request identifier of 0, since it is not
/// an answer to anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    /// A call from one side to the other.
    Request = 1,
    /// A successful answer to a request with the same identifier.
    Response = 2,
    /// A failed answer to a request with the same identifier.
    Error = 3,
    /// A display name, sent once by each side right after the handshake.
    /// See `docs/protocol.md` section 5.
    Hello = 4,
}

impl FrameKind {
    /// Read a kind from its byte.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::UnknownKind`] for any other byte. A future version
    /// may add kinds, so this is a version mismatch rather than an attack.
    pub fn from_byte(value: u8) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            3 => Ok(Self::Error),
            4 => Ok(Self::Hello),
            other => Err(FrameError::UnknownKind(other)),
        }
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// What the frame is for.
    pub kind: FrameKind,
    /// Ties a response or an error to the request that caused it.
    pub request_id: u32,
    /// The body, which the file operations layer decodes.
    pub payload: Vec<u8>,
}

/// The reason a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The stream failed.
    #[error("stream failed: {0}")]
    Io(#[from] io::Error),
    /// The declared payload length was over [`limits::MAX_FRAME_PAYLOAD`].
    #[error("payload of {0} bytes is over the limit of {max}", max = limits::MAX_FRAME_PAYLOAD)]
    PayloadTooLarge(u32),
    /// The kind byte named nothing this version knows.
    #[error("unknown frame kind {0}")]
    UnknownKind(u8),
    /// The payload did not decode.
    #[error("payload did not decode: {0}")]
    Wire(#[from] WireError),
}

/// The bytes before the payload: length, kind, and request identifier.
const HEADER_LEN: usize = 4 + 1 + 4;

/// Write one frame.
///
/// # Errors
///
/// Returns [`FrameError::PayloadTooLarge`] when the payload is over the limit,
/// and [`FrameError::Io`] when the stream fails.
pub fn write_frame(out: &mut impl Write, frame: &Frame) -> Result<(), FrameError> {
    let len = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX);
    if len > limits::MAX_FRAME_PAYLOAD {
        return Err(FrameError::PayloadTooLarge(len));
    }
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&len.to_be_bytes());
    header[4] = frame.kind as u8;
    header[5..9].copy_from_slice(&frame.request_id.to_be_bytes());

    // One buffer holding the header and the payload, so the socket sees one
    // write instead of two. Two writes back to back meet Nagle's algorithm
    // on the sender and delayed acknowledgement on the receiver, which costs
    // tens of milliseconds per frame on a loopback connection.
    //
    // `frame` is a borrow, so the payload is copied into this buffer rather
    // than moved. The copy costs microseconds even at the largest allowed
    // payload; the extra write it replaces cost tens of milliseconds, so the
    // copy is the right trade.
    let mut buf = Vec::with_capacity(HEADER_LEN + frame.payload.len());
    buf.extend_from_slice(&header);
    buf.extend_from_slice(&frame.payload);
    out.write_all(&buf)?;
    out.flush()?;
    Ok(())
}

/// Read one frame.
///
/// Blocks until a whole frame arrives.
///
/// # Errors
///
/// Returns [`FrameError::PayloadTooLarge`] when the peer declares a payload
/// over the limit. The connection should then be dropped, because the two
/// sides no longer agree on the format. Returns [`FrameError::Io`] with kind
/// [`io::ErrorKind::UnexpectedEof`] when the stream ends between frames.
pub fn read_frame(input: &mut impl Read) -> Result<Frame, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    input.read_exact(&mut header)?;

    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    if len > limits::MAX_FRAME_PAYLOAD {
        // Checked before allocating, so a four byte lie costs nothing.
        return Err(FrameError::PayloadTooLarge(len));
    }
    let kind = FrameKind::from_byte(header[4])?;
    let request_id = u32::from_be_bytes([header[5], header[6], header[7], header[8]]);

    let mut payload = vec![0u8; len as usize];
    input.read_exact(&mut payload)?;
    Ok(Frame {
        kind,
        request_id,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::{Frame, FrameError, FrameKind, read_frame, write_frame};
    use crate::limits;
    use std::io::{self, ErrorKind, Write};

    fn frame(payload: Vec<u8>) -> Frame {
        Frame {
            kind: FrameKind::Request,
            request_id: 42,
            payload,
        }
    }

    /// A writer that counts how many times `write` is called, and keeps the
    /// bytes it was given.
    ///
    /// It always accepts the whole buffer in one call, so a caller such as
    /// `write_all` never has to call `write` twice for one call of its own.
    #[derive(Default)]
    struct CountingWriter {
        calls: usize,
        bytes: Vec<u8>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writing_one_frame_calls_write_exactly_once() {
        let original = frame(b"hello".to_vec());
        let mut writer = CountingWriter::default();
        write_frame(&mut writer, &original).unwrap();

        assert_eq!(
            writer.calls, 1,
            "the header and the payload must share one write"
        );
        assert_eq!(read_frame(&mut writer.bytes.as_slice()).unwrap(), original);
    }

    #[test]
    fn a_frame_survives_a_round_trip() {
        let original = frame(b"hello".to_vec());
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &original).unwrap();
        let decoded = read_frame(&mut buffer.as_slice()).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn an_empty_payload_survives_a_round_trip() {
        let original = frame(Vec::new());
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &original).unwrap();
        assert_eq!(read_frame(&mut buffer.as_slice()).unwrap(), original);
    }

    #[test]
    fn frames_keep_their_order_and_identifiers() {
        let mut buffer = Vec::new();
        for id in 0..5u8 {
            write_frame(
                &mut buffer,
                &Frame {
                    kind: FrameKind::Response,
                    request_id: u32::from(id),
                    payload: vec![id],
                },
            )
            .unwrap();
        }
        let mut cursor = buffer.as_slice();
        for id in 0..5u8 {
            let f = read_frame(&mut cursor).unwrap();
            assert_eq!(f.request_id, u32::from(id));
            assert_eq!(f.payload, vec![id]);
        }
    }

    #[test]
    fn a_payload_over_the_limit_is_refused_on_write() {
        let big = frame(vec![0u8; limits::MAX_FRAME_PAYLOAD as usize + 1]);
        let mut buffer = Vec::new();
        assert!(matches!(
            write_frame(&mut buffer, &big),
            Err(FrameError::PayloadTooLarge(_))
        ));
        assert!(
            buffer.is_empty(),
            "nothing is written when the frame is refused"
        );
    }

    #[test]
    fn a_lie_about_the_length_is_caught_before_allocating() {
        // Four bytes claiming a four gibibyte payload, and nothing else.
        let mut buffer = u32::MAX.to_be_bytes().to_vec();
        buffer.push(FrameKind::Request as u8);
        buffer.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            read_frame(&mut buffer.as_slice()),
            Err(FrameError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn kind_4_decodes_to_hello() {
        assert_eq!(FrameKind::from_byte(4).unwrap(), FrameKind::Hello);
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mut buffer = 0u32.to_be_bytes().to_vec();
        buffer.push(200);
        buffer.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            read_frame(&mut buffer.as_slice()),
            Err(FrameError::UnknownKind(200))
        ));
    }

    #[test]
    fn a_stream_that_ends_mid_frame_is_an_error() {
        let original = frame(b"abcdefgh".to_vec());
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &original).unwrap();
        buffer.truncate(buffer.len() - 3);
        match read_frame(&mut buffer.as_slice()) {
            Err(FrameError::Io(e)) => assert_eq!(e.kind(), ErrorKind::UnexpectedEof),
            other => panic!("expected an end of file error, got {other:?}"),
        }
    }

    #[test]
    fn a_payload_at_exactly_the_limit_is_allowed() {
        let big = frame(vec![7u8; limits::MAX_FRAME_PAYLOAD as usize]);
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &big).unwrap();
        assert_eq!(read_frame(&mut buffer.as_slice()).unwrap(), big);
    }
}
