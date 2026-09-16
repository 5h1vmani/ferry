//! `IK`, the QR pairing pattern, moved out of `noise.rs` to keep that file
//! shorter. Nothing here changed when it moved: same types, same
//! functions, same doc comments. The plain helpers this needs, such as
//! `send_handshake` and `receive_handshake`, stay in `noise.rs`, shared with
//! the code pairing pattern there.

use std::fmt;
use std::io::{Read, Write};

use crate::peers::DeviceKind;
use crate::rpc::{decode_hello_payload, encode_hello_payload};

use super::{
    MAX_HANDSHAKE_MESSAGE, NoiseError, PATTERN_QR, PublicKey, QR_NONCE_LEN, SecureStream,
    StaticKey, constant_time_eq, peer_key, receive_handshake, send_handshake,
};

/// What a completed QR pairing produced, from the responder's side.
///
/// Unlike [`super::Paired`], there is no code to show: `IK`'s key binding is
/// what makes this handshake safe, not a number a person compares. The
/// nonce and hello arrived in message one, decoded here, so the caller can
/// show `Requested { name, transport }` before anyone confirms anything.
pub struct IkAccepted {
    /// The encrypted channel, ready to carry frames.
    pub stream: SecureStream,
    /// The initiator's static public key.
    pub peer: PublicKey,
    /// The nonce from the offer this initiator says it scanned. The caller
    /// checks this against the offer it is currently showing; this function
    /// only checks it against `expected_nonce`, if one was given.
    pub nonce: [u8; QR_NONCE_LEN],
    /// The initiator's display name, from its hello.
    pub name: String,
    /// The initiator's device kind, from its hello.
    pub kind: DeviceKind,
}

impl fmt::Debug for IkAccepted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IkAccepted")
            .field("peer", &self.peer)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Accept a QR pairing handshake, as the side whose static key was in the QR
/// code.
///
/// `expected_nonce` is the nonce of the offer this side is currently
/// showing, or `None` when it is not offering to pair by QR at all. Either
/// way, message one is read and decrypted first, so a caller cannot tell
/// from the outside whether a wrong nonce or no offer at all caused the
/// refusal that follows: both look like an ordinary handshake failure.
///
/// # Errors
///
/// Returns [`NoiseError::UnknownOffer`] when the initiator's nonce does not
/// equal `expected_nonce`, including when `expected_nonce` is `None`.
/// Returns [`NoiseError::BadHello`] when the payload after the nonce does
/// not decode as a hello. Returns [`NoiseError::Crypto`] when the initiator
/// does not hold the private key matching the static key it sent.
pub fn pair_ik_as_responder(
    mut stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    expected_nonce: Option<&[u8; QR_NONCE_LEN]>,
    prologue: &[u8],
) -> Result<IkAccepted, NoiseError> {
    let params = PATTERN_QR.parse().map_err(|_| NoiseError::BadPattern)?;
    let mut state = snow::Builder::new(params)
        .local_private_key(key.private_bytes())?
        .prologue(prologue)?
        .build_responder()?;

    let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
    let incoming = receive_handshake(&mut stream)?;
    let n = state.read_message(&incoming, &mut buf)?;
    if n < QR_NONCE_LEN {
        return Err(NoiseError::BadHandshakePayload);
    }
    let mut nonce = [0u8; QR_NONCE_LEN];
    nonce.copy_from_slice(&buf[..QR_NONCE_LEN]);
    let matches = expected_nonce.is_some_and(|expected| constant_time_eq(expected, &nonce));
    if !matches {
        return Err(NoiseError::UnknownOffer);
    }
    let (name, kind) = decode_hello_payload(&buf[QR_NONCE_LEN..n])?;

    let n = state.write_message(&[], &mut buf)?;
    send_handshake(&mut stream, &buf[..n])?;

    let peer = peer_key(&state)?;
    let transport = state.into_transport_mode()?;
    Ok(IkAccepted {
        stream: SecureStream::new(stream, transport),
        peer,
        nonce,
        name,
        kind,
    })
}

/// Run a QR pairing handshake, as the side that scanned the code.
///
/// `responder` is the static key read from the QR code, and `nonce` is the
/// offer's nonce, both decoded from [`crate::offer`]. `my_name` and `my_kind`
/// travel in message one, encoded the same way [`crate::rpc::exchange_hello`]
/// encodes them, so the responder can show who is asking before anyone
/// confirms anything, without a second hello.
///
/// # Errors
///
/// Returns [`NoiseError::Crypto`] when the responder does not hold the
/// private key matching `responder`, which is what makes an attacker unable
/// to complete this handshake without it.
pub fn pair_ik_as_initiator(
    mut stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    responder: &PublicKey,
    nonce: &[u8; QR_NONCE_LEN],
    my_name: &str,
    my_kind: DeviceKind,
    prologue: &[u8],
) -> Result<SecureStream, NoiseError> {
    let params = PATTERN_QR.parse().map_err(|_| NoiseError::BadPattern)?;
    let mut state = snow::Builder::new(params)
        .local_private_key(key.private_bytes())?
        .remote_public_key(responder.as_bytes())?
        .prologue(prologue)?
        .build_initiator()?;

    let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
    let mut payload = Vec::with_capacity(QR_NONCE_LEN + 4 + my_name.len() + 1);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&encode_hello_payload(my_name, my_kind)?);
    let n = state.write_message(&payload, &mut buf)?;
    send_handshake(&mut stream, &buf[..n])?;

    let incoming = receive_handshake(&mut stream)?;
    state.read_message(&incoming, &mut buf)?;
    let transport = state.into_transport_mode()?;
    Ok(SecureStream::new(stream, transport))
}
