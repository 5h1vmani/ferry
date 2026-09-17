//! Protocol version negotiation.
//!
//! Two devices may run different builds. Before anything else happens they say
//! which versions they support, and both settle on the highest version they
//! share. Since version 3 they also say which Noise pattern the initiator is
//! about to run, so the responder can build the right handshake state before
//! it reads a single byte of it.
//!
//! This exchange happens in the clear, because it comes before the encrypted
//! channel exists. That would normally let an attacker force both sides down to
//! an old, weaker version, or claim one pattern and run another. It cannot
//! happen here, because the exact bytes exchanged become the Noise prologue.
//!
//! A Noise prologue is mixed into the handshake hash. If an attacker changes
//! one byte of the version exchange, the two sides compute different handshake
//! hashes, and the handshake fails. So the version exchange is unauthenticated
//! when it happens, and authenticated a moment later.

use std::io::{self, Read, Write};

/// The bytes that start every Ferry connection.
///
/// A wrong magic value usually means something other than Ferry is listening
/// on the port.
pub const MAGIC: [u8; 5] = *b"FERRY";

/// The oldest protocol version this build can speak.
///
/// Version 3 added the mode byte below. Both the oldest and the newest
/// version moved from 2 to 3 together, in one step, the same way they moved
/// from 1 to 2: version 3 changes the wire, so no build in the field should
/// read version 3 bytes as version 2, and a stale build must fail with
/// `NoSharedVersion` rather than misread the extra byte.
pub const VERSION_MIN: u16 = 3;

/// The newest protocol version this build can speak.
pub const VERSION_MAX: u16 = 3;

/// Which Noise pattern the initiator is about to run.
///
/// Sent as one byte, right after the version, by both sides. Only the
/// initiator's byte means anything: it is the side that picks a pattern, and
/// the responder must build a handshake state for that exact pattern before
/// it can read message one at all. The responder's own byte is a filler, sent
/// only to keep the exchange a fixed length in both directions; send
/// [`Mode::Connect`] for it. See [`Agreed::mode`].
///
/// `docs/engine-contract.md` item 12: before this byte existed, the accept
/// path could only guess which pattern was coming from its own local state
/// (open to pairing, or not). That guess cannot tell `XX` pairing by code
/// apart from `IK` pairing by QR, both of which can be open at once as far as
/// the wire is concerned. Binding the choice into the prologue also closes a
/// pattern-confusion gap the same way binding the version already closes a
/// downgrade gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Run `KK`, using stored static keys. The ordinary case, once paired.
    Connect,
    /// Run `XX`, pairing by a six digit code shown on both screens.
    PairByCode,
    /// Run `IK`, pairing by a code scanned from the other screen.
    PairByQr,
}

impl Mode {
    fn to_byte(self) -> u8 {
        match self {
            Self::Connect => 0,
            Self::PairByCode => 1,
            Self::PairByQr => 2,
        }
    }

    fn from_byte(value: u8) -> Result<Self, VersionError> {
        match value {
            0 => Ok(Self::Connect),
            1 => Ok(Self::PairByCode),
            2 => Ok(Self::PairByQr),
            other => Err(VersionError::UnknownMode(other)),
        }
    }
}

/// Which side of the connection this is.
///
/// The role fixes the order of the two versions in the prologue, so that both
/// sides build the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The side that opened the connection.
    Initiator,
    /// The side that accepted it.
    Responder,
}

/// The reason version negotiation failed.
#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    /// The stream failed.
    #[error("stream failed: {0}")]
    Io(#[from] io::Error),
    /// The first bytes were not [`MAGIC`].
    #[error("not a Ferry connection, first bytes were {0:?}")]
    NotFerry([u8; 5]),
    /// No version is supported by both sides.
    #[error("no shared version, this build has {ours} and the peer has {theirs}")]
    NoSharedVersion {
        /// The newest version this build supports.
        ours: u16,
        /// The newest version the peer claims to support.
        theirs: u16,
    },
    /// The mode byte named nothing this build knows.
    #[error("the mode byte {0} names no known pattern")]
    UnknownMode(u8),
}

/// What both sides agreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreed {
    /// The version both sides will speak.
    pub version: u16,
    /// The pattern the initiator is about to run, decoded from whichever
    /// side's bytes are the initiator's: this side's own, sent value when
    /// `role` is [`Role::Initiator`], the peer's received value when it is
    /// [`Role::Responder`].
    pub mode: Mode,
    /// The exact bytes exchanged, in a fixed order.
    ///
    /// Pass this to the Noise handshake as the prologue. Doing so binds the
    /// version exchange, and the mode byte, to the handshake, which stops an
    /// attacker from forcing an old version or a different pattern.
    pub prologue: Vec<u8>,
}

/// Exchange versions and a mode, and agree on a version.
///
/// Both sides send first and then read, so neither waits on the other. `mode`
/// is this side's own request when `role` is [`Role::Initiator`]; when `role`
/// is [`Role::Responder`], this side has nothing of its own to request, so
/// pass [`Mode::Connect`] as the filler byte and read [`Agreed::mode`] for
/// what the peer actually asked for.
///
/// # Errors
///
/// Returns [`VersionError::NotFerry`] when the peer is not speaking this
/// protocol, [`VersionError::NoSharedVersion`] when no version is shared, and
/// [`VersionError::UnknownMode`] when the initiator's mode byte names nothing
/// this build knows.
pub fn negotiate(
    stream: &mut (impl Read + Write),
    role: Role,
    mode: Mode,
) -> Result<Agreed, VersionError> {
    let mut sent = Vec::with_capacity(8);
    sent.extend_from_slice(&MAGIC);
    sent.extend_from_slice(&VERSION_MAX.to_be_bytes());
    sent.push(mode.to_byte());
    stream.write_all(&sent)?;
    stream.flush()?;

    let mut received = [0u8; 8];
    stream.read_exact(&mut received)?;

    let mut magic = [0u8; 5];
    magic.copy_from_slice(&received[0..5]);
    if magic != MAGIC {
        return Err(VersionError::NotFerry(magic));
    }
    let theirs = u16::from_be_bytes([received[5], received[6]]);

    let version = VERSION_MAX.min(theirs);
    if version < VERSION_MIN || theirs < VERSION_MIN {
        return Err(VersionError::NoSharedVersion {
            ours: VERSION_MAX,
            theirs,
        });
    }

    // The prologue always lists the initiator's offer first, so both sides
    // build identical bytes whichever end they are. The initiator's mode
    // byte, likewise, is always the one that decides `Agreed::mode`.
    let (first, second) = match role {
        Role::Initiator => (sent.as_slice(), received.as_slice()),
        Role::Responder => (received.as_slice(), sent.as_slice()),
    };
    let initiator_mode = Mode::from_byte(first[7])?;
    let mut prologue = Vec::with_capacity(16);
    prologue.extend_from_slice(first);
    prologue.extend_from_slice(second);

    Ok(Agreed {
        version,
        mode: initiator_mode,
        prologue,
    })
}

#[cfg(test)]
mod tests {
    use super::{Agreed, MAGIC, Mode, Role, VERSION_MAX, VersionError, negotiate};
    use crate::transport::loopback;
    use std::io::Write;

    fn negotiate_both_ends_with(initiator_mode: Mode) -> (Agreed, Agreed) {
        let (mut a, mut b) = loopback();
        let responder =
            std::thread::spawn(move || negotiate(&mut b, Role::Responder, Mode::Connect).unwrap());
        let initiator = negotiate(&mut a, Role::Initiator, initiator_mode).unwrap();
        (initiator, responder.join().unwrap())
    }

    fn negotiate_both_ends() -> (Agreed, Agreed) {
        negotiate_both_ends_with(Mode::Connect)
    }

    #[test]
    fn both_sides_agree_on_a_version() {
        let (initiator, responder) = negotiate_both_ends();
        assert_eq!(initiator.version, VERSION_MAX);
        assert_eq!(responder.version, VERSION_MAX);
    }

    #[test]
    fn both_sides_build_the_same_prologue() {
        // If these differ, the Noise handshake fails and nothing works. The
        // prologue is what makes the cleartext version exchange safe.
        let (initiator, responder) = negotiate_both_ends();
        assert_eq!(initiator.prologue, responder.prologue);
        assert_eq!(initiator.prologue.len(), 16);
    }

    #[test]
    fn both_sides_agree_on_the_initiators_mode() {
        // The responder sends `Mode::Connect` as a filler, but what the
        // initiator asked for is what `Agreed::mode` reports on both ends.
        let (initiator, responder) = negotiate_both_ends_with(Mode::PairByQr);
        assert_eq!(initiator.mode, Mode::PairByQr);
        assert_eq!(responder.mode, Mode::PairByQr);
    }

    #[test]
    fn an_unknown_mode_byte_is_refused() {
        let (mut a, mut b) = loopback();
        let other = std::thread::spawn(move || {
            let mut sent = Vec::with_capacity(8);
            sent.extend_from_slice(&MAGIC);
            sent.extend_from_slice(&VERSION_MAX.to_be_bytes());
            sent.push(9); // Names no `Mode` this build knows.
            b.write_all(&sent).unwrap();
            // No sleep needed: loopback's Pipe drains its buffer before it
            // ever reports end of file, so a write already queues these
            // bytes for the reader whether or not this end closes right
            // after (`transport.rs`, `Pipe::read`).
        });
        match negotiate(&mut a, Role::Responder, Mode::Connect) {
            Err(VersionError::UnknownMode(9)) => {}
            other => panic!("expected an unknown mode refusal, got {other:?}"),
        }
        other.join().unwrap();
    }

    #[test]
    fn a_peer_speaking_something_else_is_refused() {
        let (mut a, mut b) = loopback();
        let other = std::thread::spawn(move || {
            b.write_all(b"HTTP/1.1 200 OK").unwrap();
            // No sleep needed: see the note in `an_unknown_mode_byte_is_refused`.
        });
        match negotiate(&mut a, Role::Initiator, Mode::Connect) {
            Err(VersionError::NotFerry(got)) => assert_ne!(got, MAGIC),
            other => panic!("expected a magic mismatch, got {other:?}"),
        }
        other.join().unwrap();
    }

    #[test]
    fn a_peer_offering_only_version_one_is_refused() {
        let (mut a, mut b) = loopback();
        let other = std::thread::spawn(move || {
            let mut sent = Vec::with_capacity(8);
            sent.extend_from_slice(&MAGIC);
            sent.extend_from_slice(&1u16.to_be_bytes());
            sent.push(0);
            b.write_all(&sent).unwrap();
            // No sleep needed: see the note in `an_unknown_mode_byte_is_refused`.
        });
        match negotiate(&mut a, Role::Initiator, Mode::Connect) {
            Err(VersionError::NoSharedVersion { ours, theirs }) => {
                assert_eq!(ours, VERSION_MAX);
                assert_eq!(theirs, 1);
            }
            other => panic!("expected NoSharedVersion, got {other:?}"),
        }
        other.join().unwrap();
    }
}
