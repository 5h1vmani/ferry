//! The encrypted channel, and pairing.
//!
//! Ferry uses the Noise protocol framework. Pairing runs the `XX` pattern once.
//! Every connection after that runs `KK`, using the static public key each
//! device stored during pairing.
//!
//! # Why pairing needs more than a short code
//!
//! In `XX` the initiator sends its static public key in message three, after it
//! has seen every other input. An attacker in the middle can therefore generate
//! static keys until the code shown on one screen matches the code it is
//! showing on the other. A six digit code needs about one million key
//! generations to forge, which takes seconds.
//!
//! So each side commits before it sees the other side's input:
//!
//! 1. The initiator commits to `BLAKE3(its static key || nonce_a)` in message
//!    one. A commitment reveals nothing, so it is safe in the clear.
//! 2. The responder sends `nonce_b` in message two.
//! 3. The initiator reveals `nonce_a` in message three. The responder checks the
//!    commitment and aborts on a mismatch.
//! 4. Both sides derive the code from the handshake hash and both nonces.
//!
//! Neither side can grind. The initiator is locked in before it learns
//! `nonce_b`. The responder never learns `nonce_a` before it must choose. See
//! decision record 6.

use std::fmt;
use std::io::{self, Read, Write};

use zeroize::Zeroize;

use crate::limits;
use crate::peers::DeviceKind;
use crate::rpc::{RpcError, decode_hello_payload, encode_hello_payload};

/// The Noise pattern used the first time two devices meet by code.
const PATTERN_PAIR: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// The Noise pattern used for every connection after pairing.
const PATTERN_CONNECT: &str = "Noise_KK_25519_ChaChaPoly_BLAKE2s";

/// The Noise pattern used the first time two devices meet by QR code.
///
/// The initiator already knows the responder's static key, read from the QR
/// code, so it sends its own key in message one instead of waiting for
/// message three the way `XX` does. This is what makes the handshake safe
/// without a commit-and-reveal code: an active attacker cannot complete it
/// without the responder's real private key. See
/// `docs/engine-contract.md` item 12.
const PATTERN_QR: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

/// How many bytes the QR pairing nonce is.
///
/// Distinct from the 32 byte commit-and-reveal nonces `XX` pairing uses.
/// This nonce is not there to stop grinding; `IK`'s key binding already does
/// that. It is there so a photographed QR code cannot be replayed: it is
/// single use and dies with the offer. `docs/protocol.md` section 4.
pub const QR_NONCE_LEN: usize = 16;

/// Separates this hash from every other use of BLAKE3 in the project.
const CODE_CONTEXT: &[u8] = b"ferry-pairing-code-v1";

/// The largest handshake message this build will read.
///
/// Real handshake messages are well under 200 bytes. The cap stops a peer from
/// making the other side hold a large buffer before it has proved anything.
const MAX_HANDSHAKE_MESSAGE: usize = 1024;

/// A device's long-lived public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A device's long-lived key pair.
///
/// The private key is wiped when this value is dropped. It has to sit in
/// memory in the clear, because the Noise handshake performs Diffie-Hellman
/// with it, and neither Android Keystore nor the Secure Enclave will release
/// raw key bytes for that.
pub struct StaticKey {
    private: [u8; 32],
    public: PublicKey,
}

impl fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The private half must never reach a log.
        f.debug_struct("StaticKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl Drop for StaticKey {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}

impl StaticKey {
    /// Create a new random key pair.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Crypto`] when the underlying library cannot
    /// produce a key.
    pub fn generate() -> Result<Self, NoiseError> {
        let params = PATTERN_PAIR.parse().map_err(|_| NoiseError::BadPattern)?;
        let keypair = snow::Builder::new(params).generate_keypair()?;
        Ok(Self::from_parts(&keypair.private, &keypair.public))
    }

    /// Rebuild a key pair from stored bytes.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::BadKeyLength`] when either half is not 32 bytes.
    pub fn from_stored(private: &[u8], public: &[u8]) -> Result<Self, NoiseError> {
        if private.len() != 32 || public.len() != 32 {
            return Err(NoiseError::BadKeyLength);
        }
        Ok(Self::from_parts(private, public))
    }

    fn from_parts(private: &[u8], public: &[u8]) -> Self {
        let mut p = [0u8; 32];
        let mut q = [0u8; 32];
        p.copy_from_slice(private);
        q.copy_from_slice(public);
        Self {
            private: p,
            public: PublicKey(q),
        }
    }

    /// This device's public key. Safe to show and to store.
    #[must_use]
    pub fn public(&self) -> PublicKey {
        self.public
    }

    /// The private half, for writing to secure storage.
    ///
    /// Handle the result carefully and wipe it when done.
    #[must_use]
    pub fn private_bytes(&self) -> &[u8; 32] {
        &self.private
    }
}

/// The number two people compare during pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingCode(u32);

impl PairingCode {
    /// How many digits the code has.
    pub const DIGITS: u32 = 6;

    /// The code as a number below one million.
    #[must_use]
    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:06}", self.0)
    }
}

/// The reason a handshake failed.
#[derive(Debug, thiserror::Error)]
pub enum NoiseError {
    /// The stream failed.
    #[error("stream failed: {0}")]
    Io(#[from] io::Error),
    /// The Noise library refused something.
    #[error("handshake failed: {0}")]
    Crypto(#[from] snow::Error),
    /// A pattern string in this crate is wrong. This is a bug, not an attack.
    #[error("the Noise pattern string is wrong")]
    BadPattern,
    /// A stored key was not 32 bytes.
    #[error("a stored key was not 32 bytes")]
    BadKeyLength,
    /// The initiator revealed a nonce that does not match what it committed to.
    ///
    /// Someone tried to change their identity after seeing the other side's
    /// input. Abort and show nothing.
    #[error("the peer changed its identity after committing to it")]
    CommitmentMismatch,
    /// A handshake message was larger than this build will read.
    #[error("handshake message of {0} bytes is over the limit")]
    HandshakeMessageTooLarge(usize),
    /// A handshake payload was not the size the pattern requires.
    #[error("a handshake payload was the wrong size")]
    BadHandshakePayload,
    /// The peer finished the handshake without offering a static key.
    #[error("the peer offered no static key")]
    MissingPeerKey,
    /// An `IK` message one's nonce did not match the offer this side is
    /// currently showing, or nothing is being offered at all.
    ///
    /// Covers both a wrong nonce, which means a stale or foreign QR code, and
    /// no live offer, which means this side is not currently offering to
    /// pair by QR at all. Neither tells the initiator which; both look like a
    /// plain handshake failure from outside.
    #[error("the initiator's nonce does not match a live offer")]
    UnknownOffer,
    /// An `IK` message one's hello payload did not decode.
    #[error("the pairing hello did not decode: {0}")]
    BadHello(#[from] RpcError),
}

/// What a completed pairing produced.
pub struct Paired {
    /// The encrypted channel, ready to carry frames.
    pub stream: SecureStream,
    /// The peer's static public key. Store it only after the code is confirmed.
    pub peer: PublicKey,
    /// The code to show the user. The other device must show the same one.
    pub code: PairingCode,
}

impl fmt::Debug for Paired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paired")
            .field("peer", &self.peer)
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

/// Compares two QR pairing nonces without branching on where they first
/// differ, so a timing measurement cannot help a stranger guess the
/// offer's nonce one byte at a time. XOR-folds every byte together rather
/// than comparing byte by byte with an early exit.
fn constant_time_eq(a: &[u8; QR_NONCE_LEN], b: &[u8; QR_NONCE_LEN]) -> bool {
    let mut differs: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        differs |= x ^ y;
    }
    differs == 0
}

fn random_nonce() -> Result<[u8; 32], NoiseError> {
    let mut out = [0u8; 32];
    getrandom::fill(&mut out)
        .map_err(|e| NoiseError::Io(io::Error::other(format!("no randomness: {e}"))))?;
    Ok(out)
}

fn commitment(public: &PublicKey, nonce: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ferry-pairing-commitment-v1");
    hasher.update(public.as_bytes());
    hasher.update(nonce);
    *hasher.finalize().as_bytes()
}

fn derive_code(handshake_hash: &[u8], nonce_a: &[u8; 32], nonce_b: &[u8; 32]) -> PairingCode {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CODE_CONTEXT);
    hasher.update(handshake_hash);
    hasher.update(nonce_a);
    hasher.update(nonce_b);
    let digest = hasher.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest.as_bytes()[..8]);
    // Eight bytes reduced to six digits. The bias is far below one part in a
    // trillion, so it gives away nothing.
    let value = u64::from_be_bytes(first) % 1_000_000;
    PairingCode(u32::try_from(value).unwrap_or(0))
}

fn send_handshake(out: &mut impl Write, message: &[u8]) -> Result<(), NoiseError> {
    let len = u16::try_from(message.len())
        .map_err(|_| NoiseError::HandshakeMessageTooLarge(message.len()))?;
    // One buffer for the length prefix and the message, so the socket sees
    // one write instead of two. See `SecureStream::write` for why this
    // matters: two writes back to back can each cost tens of milliseconds
    // on a loopback connection. `message` is a caller-owned slice into a
    // reused buffer, so it is copied here; a handshake message is at most
    // `MAX_HANDSHAKE_MESSAGE` bytes, so the copy costs nothing worth
    // measuring next to the write it replaces.
    let mut buf = Vec::with_capacity(2 + message.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(message);
    out.write_all(&buf)?;
    out.flush()?;
    Ok(())
}

fn receive_handshake(input: &mut impl Read) -> Result<Vec<u8>, NoiseError> {
    let mut header = [0u8; 2];
    input.read_exact(&mut header)?;
    let len = usize::from(u16::from_be_bytes(header));
    if len > MAX_HANDSHAKE_MESSAGE {
        return Err(NoiseError::HandshakeMessageTooLarge(len));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    Ok(body)
}

fn peer_key(state: &snow::HandshakeState) -> Result<PublicKey, NoiseError> {
    let raw = state
        .get_remote_static()
        .ok_or(NoiseError::MissingPeerKey)?;
    if raw.len() != 32 {
        return Err(NoiseError::BadKeyLength);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(raw);
    Ok(PublicKey(out))
}

/// Pair with a peer, as the side that opened the connection.
///
/// The returned code must be shown to the user and confirmed on both devices
/// before [`Paired::peer`] is stored.
///
/// # Errors
///
/// Returns [`NoiseError::Crypto`] when the handshake fails, and
/// [`NoiseError::Io`] when the stream fails.
pub fn pair_as_initiator(
    mut stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    prologue: &[u8],
) -> Result<Paired, NoiseError> {
    let params = PATTERN_PAIR.parse().map_err(|_| NoiseError::BadPattern)?;
    let mut state = snow::Builder::new(params)
        .local_private_key(key.private_bytes())?
        .prologue(prologue)?
        .build_initiator()?;

    let nonce_a = random_nonce()?;
    let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];

    // Message one carries the commitment. It reveals nothing on its own.
    let commit = commitment(&key.public(), &nonce_a);
    let n = state.write_message(&commit, &mut buf)?;
    send_handshake(&mut stream, &buf[..n])?;

    // Message two carries the responder's nonce.
    let incoming = receive_handshake(&mut stream)?;
    let n = state.read_message(&incoming, &mut buf)?;
    if n != 32 {
        return Err(NoiseError::BadHandshakePayload);
    }
    let mut nonce_b = [0u8; 32];
    nonce_b.copy_from_slice(&buf[..32]);

    // Message three reveals the nonce, so the responder can check the
    // commitment against the static key it now holds.
    let n = state.write_message(&nonce_a, &mut buf)?;
    send_handshake(&mut stream, &buf[..n])?;

    let code = derive_code(state.get_handshake_hash(), &nonce_a, &nonce_b);
    let peer = peer_key(&state)?;
    let transport = state.into_transport_mode()?;
    Ok(Paired {
        stream: SecureStream::new(stream, transport),
        peer,
        code,
    })
}

/// Pair with a peer, as the side that accepted the connection.
///
/// # Errors
///
/// Returns [`NoiseError::CommitmentMismatch`] when the initiator reveals a
/// nonce that does not match what it committed to. That means someone tried to
/// change identity after seeing this side's input. Show no code, and abort.
pub fn pair_as_responder(
    mut stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    prologue: &[u8],
) -> Result<Paired, NoiseError> {
    let params = PATTERN_PAIR.parse().map_err(|_| NoiseError::BadPattern)?;
    let mut state = snow::Builder::new(params)
        .local_private_key(key.private_bytes())?
        .prologue(prologue)?
        .build_responder()?;

    let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];

    let incoming = receive_handshake(&mut stream)?;
    let n = state.read_message(&incoming, &mut buf)?;
    if n != 32 {
        return Err(NoiseError::BadHandshakePayload);
    }
    let mut commit = [0u8; 32];
    commit.copy_from_slice(&buf[..32]);

    let nonce_b = random_nonce()?;
    let n = state.write_message(&nonce_b, &mut buf)?;
    send_handshake(&mut stream, &buf[..n])?;

    let incoming = receive_handshake(&mut stream)?;
    let n = state.read_message(&incoming, &mut buf)?;
    if n != 32 {
        return Err(NoiseError::BadHandshakePayload);
    }
    let mut nonce_a = [0u8; 32];
    nonce_a.copy_from_slice(&buf[..32]);

    let peer = peer_key(&state)?;
    if commitment(&peer, &nonce_a) != commit {
        return Err(NoiseError::CommitmentMismatch);
    }

    let code = derive_code(state.get_handshake_hash(), &nonce_a, &nonce_b);
    let transport = state.into_transport_mode()?;
    Ok(Paired {
        stream: SecureStream::new(stream, transport),
        peer,
        code,
    })
}

/// What a completed QR pairing produced, from the responder's side.
///
/// Unlike [`Paired`], there is no code to show: `IK`'s key binding is what
/// makes this handshake safe, not a number a person compares. The nonce and
/// hello arrived in message one, decoded here, so the caller can show
/// `Requested { name, transport }` before anyone confirms anything.
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

/// Connect to a device that has already been paired, as the initiator.
///
/// # Errors
///
/// Returns [`NoiseError::Crypto`] when the peer does not hold the matching
/// private key, which is what makes an unpaired device unable to connect.
pub fn connect_as_initiator(
    mut stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    peer: &PublicKey,
    prologue: &[u8],
) -> Result<SecureStream, NoiseError> {
    let mut state = kk_state(key, peer, prologue, true)?;
    let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
    let n = state.write_message(&[], &mut buf)?;
    send_handshake(&mut stream, &buf[..n])?;
    let incoming = receive_handshake(&mut stream)?;
    state.read_message(&incoming, &mut buf)?;
    let transport = state.into_transport_mode()?;
    Ok(SecureStream::new(stream, transport))
}

/// Accept a connection from a device that has already been paired, checked
/// against exactly one candidate key.
///
/// A responder with more than one stored peer cannot use this directly: `KK`
/// needs the initiator's static key before it can even read message one, and
/// the stream cannot be read twice to try a second guess. See
/// [`read_kk_message_one`] and [`KkMessageOne::try_candidate`], which split
/// that read from the guess so more than one candidate can be tried against
/// it. `docs/engine-contract.md` item 16b.
///
/// # Errors
///
/// As [`connect_as_initiator`].
pub fn connect_as_responder(
    stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    peer: &PublicKey,
    prologue: &[u8],
) -> Result<SecureStream, NoiseError> {
    let (stream, _peer) =
        connect_as_responder_any(stream, key, std::slice::from_ref(peer), prologue)?;
    Ok(stream)
}

/// Accept a connection from a device that has already been paired, trying
/// each of `candidates` in turn against the one message the peer sends, and
/// binding the first that authenticates.
///
/// `docs/engine-contract.md` item 16b: a responder with more than one stored
/// peer previously guessed a single candidate, and a wrong guess failed the
/// handshake outright. Reading message one once and trying every candidate
/// against it fixes that.
///
/// # Errors
///
/// Returns [`NoiseError::Crypto`] when no candidate authenticates. The error
/// is the last candidate's; none of the earlier failures can be singled out
/// as more informative than the others.
pub fn connect_as_responder_any(
    mut stream: impl Read + Write + Send + 'static,
    key: &StaticKey,
    candidates: &[PublicKey],
    prologue: &[u8],
) -> Result<(SecureStream, PublicKey), NoiseError> {
    let first = read_kk_message_one(&mut stream)?;
    let mut last_error = NoiseError::MissingPeerKey;
    for candidate in candidates {
        match first.try_candidate(key, candidate, prologue) {
            Ok(bound) => return Ok((bound.finish(stream)?, *candidate)),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// Build the `KK` handshake state for one candidate key.
fn kk_state(
    key: &StaticKey,
    candidate: &PublicKey,
    prologue: &[u8],
    initiator: bool,
) -> Result<snow::HandshakeState, NoiseError> {
    let params = PATTERN_CONNECT
        .parse()
        .map_err(|_| NoiseError::BadPattern)?;
    let builder = snow::Builder::new(params)
        .local_private_key(key.private_bytes())?
        .remote_public_key(candidate.as_bytes())?
        .prologue(prologue)?;
    Ok(if initiator {
        builder.build_initiator()?
    } else {
        builder.build_responder()?
    })
}

/// A `KK` responder's first handshake message, read once.
///
/// The stream cannot be rewound and a handshake cannot be retried on it, so
/// trying more than one stored key means building a fresh handshake state
/// for each candidate and replaying these same bytes into it, rather than
/// reading the stream again. `KK` needs the initiator's static key to
/// decrypt message one at all, so the wrong candidate simply fails to
/// authenticate instead of silently accepting. `docs/engine-contract.md`
/// item 16b.
pub struct KkMessageOne {
    incoming: Vec<u8>,
}

/// Read a `KK` responder's first handshake message.
///
/// Call this once; try as many candidates as needed against the value it
/// returns, with [`KkMessageOne::try_candidate`].
///
/// # Errors
///
/// Returns [`NoiseError::Io`] when the stream fails, and
/// [`NoiseError::HandshakeMessageTooLarge`] when the peer claims a message
/// over [`MAX_HANDSHAKE_MESSAGE`] bytes.
pub fn read_kk_message_one(stream: &mut impl Read) -> Result<KkMessageOne, NoiseError> {
    Ok(KkMessageOne {
        incoming: receive_handshake(stream)?,
    })
}

impl KkMessageOne {
    /// Try one candidate key against the message [`read_kk_message_one`]
    /// read.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Crypto`] when `candidate` is not the device
    /// that sent message one.
    pub fn try_candidate(
        &self,
        key: &StaticKey,
        candidate: &PublicKey,
        prologue: &[u8],
    ) -> Result<BoundKk, NoiseError> {
        let mut state = kk_state(key, candidate, prologue, false)?;
        let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
        state.read_message(&self.incoming, &mut buf)?;
        Ok(BoundKk { state })
    }
}

/// A `KK` responder handshake whose candidate has authenticated. Sending
/// message two and switching to transport mode are the only steps left.
pub struct BoundKk {
    state: snow::HandshakeState,
}

impl BoundKk {
    /// Send message two and move to transport mode.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Io`] when the stream fails.
    pub fn finish(
        mut self,
        mut stream: impl Read + Write + Send + 'static,
    ) -> Result<SecureStream, NoiseError> {
        let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
        let n = self.state.write_message(&[], &mut buf)?;
        send_handshake(&mut stream, &buf[..n])?;
        let transport = self.state.into_transport_mode()?;
        Ok(SecureStream::new(stream, transport))
    }
}

/// An encrypted byte stream.
///
/// This implements [`Read`] and [`Write`], so the frame codec runs over it
/// unchanged. Framing does not know that the bytes are encrypted.
///
/// The Noise specification caps one transport message at 65535 bytes. This type
/// splits writes to fit and joins reads back, so callers never see that cap.
pub struct SecureStream {
    inner: Box<dyn ReadWrite>,
    state: snow::TransportState,
    /// Plaintext already decrypted but not yet handed to the caller.
    pending: Vec<u8>,
    /// How much of `pending` the caller has taken.
    taken: usize,
}

/// A stream this crate can own and move between threads.
trait ReadWrite: Read + Write + Send {}
impl<T: Read + Write + Send> ReadWrite for T {}

impl fmt::Debug for SecureStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecureStream").finish_non_exhaustive()
    }
}

impl SecureStream {
    fn new(inner: impl Read + Write + Send + 'static, state: snow::TransportState) -> Self {
        Self {
            inner: Box::new(inner),
            state,
            pending: Vec::new(),
            taken: 0,
        }
    }
}

impl Read for SecureStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.taken == self.pending.len() {
            let mut header = [0u8; 2];
            match self.inner.read_exact(&mut header) {
                Ok(()) => {}
                // A clean end of stream is not an error for the caller.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) => return Err(e),
            }
            let len = usize::from(u16::from_be_bytes(header));
            let mut ciphertext = vec![0u8; len];
            self.inner.read_exact(&mut ciphertext)?;

            let mut plaintext = vec![0u8; limits::MAX_NOISE_PLAINTEXT];
            let n = self
                .state
                .read_message(&ciphertext, &mut plaintext)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            plaintext.truncate(n);
            self.pending = plaintext;
            self.taken = 0;
        }

        let available = self.pending.len() - self.taken;
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.pending[self.taken..self.taken + n]);
        self.taken += n;
        Ok(n)
    }
}

impl Write for SecureStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(limits::MAX_NOISE_PLAINTEXT);

        // One buffer holds the length prefix and the ciphertext, with the
        // first 2 bytes left for the prefix. `write_message` encrypts
        // straight into its final position in the buffer, so no copy is
        // needed to join the two, and one `write_all` sends both. Two
        // writes back to back meet Nagle's algorithm on the sender and
        // delayed acknowledgement on the receiver, which costs tens of
        // milliseconds per message on a loopback connection.
        let mut buf = vec![0u8; 2 + n + 16];
        let written = self
            .state
            .write_message(&bytes[..n], &mut buf[2..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let len = u16::try_from(written)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message too long"))?;
        buf[0..2].copy_from_slice(&len.to_be_bytes());
        self.inner.write_all(&buf[..2 + written])?;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IkAccepted, MAX_HANDSHAKE_MESSAGE, NoiseError, PATTERN_PAIR, Paired, PublicKey,
        QR_NONCE_LEN, SecureStream, StaticKey, commitment, connect_as_initiator,
        connect_as_responder, pair_as_initiator, pair_as_responder, pair_ik_as_initiator,
        pair_ik_as_responder, random_nonce, read_kk_message_one, receive_handshake, send_handshake,
    };
    use crate::frame::{Frame, FrameKind, read_frame, write_frame};
    use crate::peers::DeviceKind;
    use crate::transport::{Endpoint, loopback};
    use std::io::{self, Read, Write};
    use std::sync::{Arc, Mutex};

    /// A valid Noise prologue for these low level tests: the same 16 bytes
    /// `version::negotiate` builds for two sides that both send `MAGIC`,
    /// `VERSION_MAX`, and the `Connect` mode byte (`0`). Built from those
    /// real constants, rather than typed out by hand, so a future version
    /// bump cannot leave this silently stale the way a hand written
    /// literal already once did.
    const PROLOGUE: &[u8; 16] = &{
        let mut bytes = [0u8; 16];
        let mut i = 0;
        while i < crate::version::MAGIC.len() {
            bytes[i] = crate::version::MAGIC[i];
            bytes[8 + i] = crate::version::MAGIC[i];
            i += 1;
        }
        let version_bytes = crate::version::VERSION_MAX.to_be_bytes();
        bytes[5] = version_bytes[0];
        bytes[6] = version_bytes[1];
        // Byte 7 (and byte 15) is the mode: `Mode::Connect`'s byte value,
        // 0, left as the array's own default. Every test below runs a
        // handshake pattern already agreed on, not pairing by code, so
        // this is the filler value `version::negotiate` itself sends for
        // a responder with nothing of its own to request.
        bytes[7] = 0;
        bytes[13] = version_bytes[0];
        bytes[14] = version_bytes[1];
        bytes[15] = 0;
        bytes
    };

    fn pair_over_loopback() -> (Paired, Paired) {
        let (a, b) = loopback();
        let key_b = StaticKey::generate().unwrap();
        let responder = std::thread::spawn(move || pair_as_responder(b, &key_b, PROLOGUE));
        let key_a = StaticKey::generate().unwrap();
        let initiator = pair_as_initiator(a, &key_a, PROLOGUE).unwrap();
        (initiator, responder.join().unwrap().unwrap())
    }

    /// A loopback endpoint that counts how many times `write` is called on
    /// it, shared through `counter` so the count can be read after the
    /// endpoint has been moved into a `SecureStream`.
    struct CountingEndpoint {
        inner: Endpoint,
        counter: Arc<Mutex<usize>>,
    }

    impl Read for CountingEndpoint {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.inner.read(out)
        }
    }

    impl Write for CountingEndpoint {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            *self.counter.lock().expect("counter lock poisoned") += 1;
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn pairing_shows_the_same_code_on_both_screens() {
        let (initiator, responder) = pair_over_loopback();
        assert_eq!(initiator.code, responder.code);
        assert!(initiator.code.value() < 1_000_000);
        assert_eq!(format!("{}", initiator.code).len(), 6);
    }

    #[test]
    fn each_side_learns_the_other_key() {
        let (a, b) = loopback();
        let key_b = StaticKey::generate().unwrap();
        let public_b = key_b.public();
        let responder = std::thread::spawn(move || pair_as_responder(b, &key_b, PROLOGUE).unwrap());
        let key_a = StaticKey::generate().unwrap();
        let public_a = key_a.public();
        let initiator = pair_as_initiator(a, &key_a, PROLOGUE).unwrap();
        let responder = responder.join().unwrap();

        assert_eq!(initiator.peer, public_b);
        assert_eq!(responder.peer, public_a);
    }

    #[test]
    fn two_pairings_produce_different_codes() {
        // The code depends on fresh nonces, so it must not repeat.
        let (a1, _) = pair_over_loopback();
        let (a2, _) = pair_over_loopback();
        assert_ne!(a1.code, a2.code);
    }

    #[test]
    fn data_flows_over_the_paired_channel() {
        let (mut initiator, mut responder) = pair_over_loopback();
        initiator.stream.write_all(b"hello over noise").unwrap();
        let mut buf = [0u8; 16];
        responder.stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello over noise");
    }

    #[test]
    fn writing_one_noise_message_calls_write_exactly_once() {
        let (a, b) = loopback();
        let counter = Arc::new(Mutex::new(0usize));
        let counting_a = CountingEndpoint {
            inner: a,
            counter: Arc::clone(&counter),
        };

        let key_b = StaticKey::generate().unwrap();
        let responder = std::thread::spawn(move || pair_as_responder(b, &key_b, PROLOGUE));
        let key_a = StaticKey::generate().unwrap();
        let mut initiator = pair_as_initiator(counting_a, &key_a, PROLOGUE).unwrap();
        let mut responder = responder.join().unwrap().unwrap();

        // The handshake above already wrote through the counted endpoint.
        // Only the write below, after pairing, is a transport message, so
        // the count is read on both sides of it.
        let before = *counter.lock().unwrap();
        initiator.stream.write_all(b"one message").unwrap();
        let after = *counter.lock().unwrap();

        assert_eq!(
            after - before,
            1,
            "the length prefix and the ciphertext must share one write"
        );

        let mut buf = [0u8; 11];
        responder.stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"one message");
    }

    #[test]
    fn a_payload_larger_than_one_noise_message_survives() {
        // The Noise cap is 65535 bytes. This is bigger, so it must be split
        // and joined again without the caller noticing.
        let (mut initiator, mut responder) = pair_over_loopback();
        let payload: Vec<u8> = (0..300_000)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let sender = std::thread::spawn(move || {
            initiator.stream.write_all(&payload).unwrap();
            payload
        });
        let mut received = vec![0u8; 300_000];
        responder.stream.read_exact(&mut received).unwrap();
        assert_eq!(received, sender.join().unwrap());
    }

    #[test]
    fn frames_travel_over_the_encrypted_channel() {
        // The frame codec must not need to know the stream is encrypted.
        let (mut initiator, mut responder) = pair_over_loopback();
        let sent = Frame {
            kind: FrameKind::Request,
            request_id: 9,
            payload: vec![7u8; 5000],
        };
        let expected = sent.clone();
        std::thread::spawn(move || write_frame(&mut initiator.stream, &sent).unwrap());
        assert_eq!(read_frame(&mut responder.stream).unwrap(), expected);
    }

    fn stored_pair() -> (StaticKey, PublicKey, StaticKey, PublicKey) {
        let a = StaticKey::generate().unwrap();
        let b = StaticKey::generate().unwrap();
        let (pa, pb) = (a.public(), b.public());
        (a, pa, b, pb)
    }

    #[test]
    fn a_paired_device_reconnects() {
        let (key_a, public_a, key_b, public_b) = stored_pair();
        let (ea, eb) = loopback();
        let server = std::thread::spawn(move || {
            let mut s = connect_as_responder(eb, &key_b, &public_a, PROLOGUE).unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).unwrap();
            buf
        });
        let mut client = connect_as_initiator(ea, &key_a, &public_b, PROLOGUE).unwrap();
        client.write_all(b"again").unwrap();
        assert_eq!(&server.join().unwrap(), b"again");
    }

    #[test]
    fn a_responder_with_two_candidates_authenticates_the_second() {
        // docs/engine-contract.md item 16b: the first candidate tried is a
        // decoy, standing in for a stored peer that is not the one calling.
        // Message one is read once, on this one stream, and both candidates
        // are tried against that same read.
        let (key_a, public_a, key_b, public_b) = stored_pair();
        let decoy_public = StaticKey::generate().unwrap().public();
        let (ea, mut eb) = loopback();

        let server = std::thread::spawn(move || {
            let first = read_kk_message_one(&mut eb).unwrap();
            assert!(
                first
                    .try_candidate(&key_b, &decoy_public, PROLOGUE)
                    .is_err(),
                "a candidate that never sent message one must not authenticate"
            );
            let bound = first
                .try_candidate(&key_b, &public_a, PROLOGUE)
                .expect("the second candidate is the one that actually connected");
            let mut secure = bound.finish(eb).unwrap();
            let mut buf = [0u8; 5];
            secure.read_exact(&mut buf).unwrap();
            buf
        });

        let mut client = connect_as_initiator(ea, &key_a, &public_b, PROLOGUE).unwrap();
        client.write_all(b"again").unwrap();
        assert_eq!(&server.join().unwrap(), b"again");
    }

    #[test]
    fn a_device_that_is_not_paired_cannot_connect() {
        let (_key_a, public_a, key_b, public_b) = stored_pair();
        let stranger = StaticKey::generate().unwrap();
        let (ea, eb) = loopback();
        let server = std::thread::spawn(move || {
            connect_as_responder(eb, &key_b, &public_a, PROLOGUE).map(|_| ())
        });
        // The stranger knows where to connect, but holds no matching key.
        let attempt = connect_as_initiator(ea, &stranger, &public_b, PROLOGUE);
        let server = server.join().unwrap();
        assert!(
            attempt.is_err() || server.is_err(),
            "an unpaired device must be refused"
        );
    }

    #[test]
    fn a_tampered_version_exchange_breaks_the_handshake() {
        // The prologue carries the cleartext version exchange. If an attacker
        // edits it, the two sides derive different handshake hashes.
        let (key_a, public_a, key_b, public_b) = stored_pair();
        let (ea, eb) = loopback();
        let server = std::thread::spawn(move || {
            connect_as_responder(eb, &key_b, &public_a, b"FERRY\x00\x02FERRY\x00\x02").map(|_| ())
        });
        let attempt = connect_as_initiator(ea, &key_a, &public_b, b"FERRY\x00\x02FERRY\x00\x00");
        let server = server.join().unwrap();
        assert!(
            attempt.is_err() || server.is_err(),
            "a downgrade must break the handshake"
        );
    }

    #[test]
    fn an_initiator_that_changes_identity_is_caught() {
        // This is the attack the commitment exists to stop. A dishonest
        // initiator commits to one value, then reveals a nonce that does not
        // match the static key it actually used.
        let (a, b) = loopback();
        let key_b = StaticKey::generate().unwrap();
        let responder = std::thread::spawn(move || pair_as_responder(b, &key_b, PROLOGUE));

        let key_a = StaticKey::generate().unwrap();
        let params = PATTERN_PAIR.parse().unwrap();
        let mut state = snow::Builder::new(params)
            .local_private_key(key_a.private_bytes())
            .unwrap()
            .prologue(PROLOGUE)
            .unwrap()
            .build_initiator()
            .unwrap();

        let mut endpoint: Endpoint = a;
        let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
        let honest_nonce = random_nonce().unwrap();
        let commit = commitment(&key_a.public(), &honest_nonce);
        let n = state.write_message(&commit, &mut buf).unwrap();
        send_handshake(&mut endpoint, &buf[..n]).unwrap();

        let incoming = receive_handshake(&mut endpoint).unwrap();
        state.read_message(&incoming, &mut buf).unwrap();

        // Reveal a different nonce than the one committed to.
        let other_nonce = random_nonce().unwrap();
        let n = state.write_message(&other_nonce, &mut buf).unwrap();
        send_handshake(&mut endpoint, &buf[..n]).unwrap();

        match responder.join().unwrap() {
            Err(NoiseError::CommitmentMismatch) => {}
            other => panic!("expected a commitment mismatch, got {other:?}"),
        }
    }

    struct IkPairResult {
        initiator_stream: SecureStream,
        initiator_key: PublicKey,
        accepted: IkAccepted,
    }

    fn ik_over_loopback(nonce: [u8; QR_NONCE_LEN]) -> Result<IkPairResult, NoiseError> {
        let (a, b) = loopback();
        let key_responder = StaticKey::generate().unwrap();
        let public_responder = key_responder.public();
        let responder = std::thread::spawn(move || {
            pair_ik_as_responder(b, &key_responder, Some(&nonce), PROLOGUE)
        });
        let key_initiator = StaticKey::generate().unwrap();
        let initiator_stream = pair_ik_as_initiator(
            a,
            &key_initiator,
            &public_responder,
            &nonce,
            "Pixel 3 XL",
            DeviceKind::Phone,
            PROLOGUE,
        )?;
        let accepted = responder.join().unwrap()?;
        Ok(IkPairResult {
            initiator_stream,
            initiator_key: key_initiator.public(),
            accepted,
        })
    }

    #[test]
    fn an_ik_pairing_round_trips_the_nonce_and_hello() {
        let nonce = random_nonce().unwrap()[..QR_NONCE_LEN].try_into().unwrap();
        let mut result = ik_over_loopback(nonce).unwrap();

        assert_eq!(result.accepted.peer, result.initiator_key);
        assert_eq!(result.accepted.nonce, nonce);
        assert_eq!(result.accepted.name, "Pixel 3 XL");
        assert_eq!(result.accepted.kind, DeviceKind::Phone);

        // The channel works both ways once the handshake is done.
        result
            .initiator_stream
            .write_all(b"hello from the phone")
            .unwrap();
        let mut buf = [0u8; 20];
        result.accepted.stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello from the phone");
    }

    #[test]
    fn a_wrong_nonce_is_refused() {
        let (a, b) = loopback();
        let key_responder = StaticKey::generate().unwrap();
        let public_responder = key_responder.public();
        let offered: [u8; QR_NONCE_LEN] =
            random_nonce().unwrap()[..QR_NONCE_LEN].try_into().unwrap();
        let responder = std::thread::spawn(move || {
            pair_ik_as_responder(b, &key_responder, Some(&offered), PROLOGUE)
        });

        // The initiator scanned a stale or foreign code: a different nonce
        // than the one this side is actually offering.
        let scanned: [u8; QR_NONCE_LEN] =
            random_nonce().unwrap()[..QR_NONCE_LEN].try_into().unwrap();
        let key_initiator = StaticKey::generate().unwrap();
        let _ = pair_ik_as_initiator(
            a,
            &key_initiator,
            &public_responder,
            &scanned,
            "Pixel 3 XL",
            DeviceKind::Phone,
            PROLOGUE,
        );

        match responder.join().unwrap() {
            Err(NoiseError::UnknownOffer) => {}
            other => panic!("expected an unknown offer refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_responder_that_is_not_offering_refuses() {
        let (a, b) = loopback();
        let key_responder = StaticKey::generate().unwrap();
        let public_responder = key_responder.public();
        // `None`: nothing is being offered right now.
        let responder =
            std::thread::spawn(move || pair_ik_as_responder(b, &key_responder, None, PROLOGUE));

        let nonce: [u8; QR_NONCE_LEN] = random_nonce().unwrap()[..QR_NONCE_LEN].try_into().unwrap();
        let key_initiator = StaticKey::generate().unwrap();
        let _ = pair_ik_as_initiator(
            a,
            &key_initiator,
            &public_responder,
            &nonce,
            "Pixel 3 XL",
            DeviceKind::Phone,
            PROLOGUE,
        );

        match responder.join().unwrap() {
            Err(NoiseError::UnknownOffer) => {}
            other => panic!("expected an unknown offer refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_handshake_message_is_refused_before_reading() {
        let (mut a, mut b) = loopback();
        let claimed = u16::try_from(MAX_HANDSHAKE_MESSAGE + 1).unwrap();
        a.write_all(&claimed.to_be_bytes()).unwrap();
        match receive_handshake(&mut b) {
            Err(NoiseError::HandshakeMessageTooLarge(n)) => {
                assert_eq!(n, MAX_HANDSHAKE_MESSAGE + 1);
            }
            other => panic!("expected a size refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_stored_key_round_trips() {
        let key = StaticKey::generate().unwrap();
        let restored =
            StaticKey::from_stored(key.private_bytes(), key.public().as_bytes()).unwrap();
        assert_eq!(restored.public(), key.public());
        assert!(StaticKey::from_stored(&[0u8; 31], &[0u8; 32]).is_err());
    }

    #[test]
    fn the_private_key_never_reaches_a_debug_line() {
        let key = StaticKey::generate().unwrap();
        let printed = format!("{key:?}");
        let secret = format!("{:?}", key.private_bytes());
        assert!(
            !printed.contains(&secret),
            "the private key leaked into Debug output"
        );
    }
}
