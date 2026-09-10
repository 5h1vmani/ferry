//! Protocol version negotiation.
//!
//! Two devices may run different builds. Before anything else happens they say
//! which versions they support, and both settle on the highest version they
//! share.
//!
//! This exchange happens in the clear, because it comes before the encrypted
//! channel exists. That would normally let an attacker force both sides down to
//! an old, weaker version. It cannot happen here, because the exact bytes
//! exchanged become the Noise prologue.
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
pub const VERSION_MIN: u16 = 2;

/// The newest protocol version this build can speak.
pub const VERSION_MAX: u16 = 2;

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
}

/// What both sides agreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreed {
    /// The version both sides will speak.
    pub version: u16,
    /// The exact bytes exchanged, in a fixed order.
    ///
    /// Pass this to the Noise handshake as the prologue. Doing so binds the
    /// version exchange to the handshake, which stops an attacker from forcing
    /// an old version.
    pub prologue: Vec<u8>,
}

/// Exchange versions and agree on one.
///
/// Both sides send first and then read, so neither waits on the other.
///
/// # Errors
///
/// Returns [`VersionError::NotFerry`] when the peer is not speaking this
/// protocol, and [`VersionError::NoSharedVersion`] when no version is shared.
pub fn negotiate(stream: &mut (impl Read + Write), role: Role) -> Result<Agreed, VersionError> {
    let mut sent = Vec::with_capacity(7);
    sent.extend_from_slice(&MAGIC);
    sent.extend_from_slice(&VERSION_MAX.to_be_bytes());
    stream.write_all(&sent)?;
    stream.flush()?;

    let mut received = [0u8; 7];
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
    // build identical bytes whichever end they are.
    let (first, second) = match role {
        Role::Initiator => (sent.as_slice(), received.as_slice()),
        Role::Responder => (received.as_slice(), sent.as_slice()),
    };
    let mut prologue = Vec::with_capacity(14);
    prologue.extend_from_slice(first);
    prologue.extend_from_slice(second);

    Ok(Agreed { version, prologue })
}

#[cfg(test)]
mod tests {
    use super::{Agreed, MAGIC, Role, VERSION_MAX, VersionError, negotiate};
    use crate::transport::loopback;
    use std::io::Write;

    fn negotiate_both_ends() -> (Agreed, Agreed) {
        let (mut a, mut b) = loopback();
        let responder = std::thread::spawn(move || negotiate(&mut b, Role::Responder).unwrap());
        let initiator = negotiate(&mut a, Role::Initiator).unwrap();
        (initiator, responder.join().unwrap())
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
        assert_eq!(initiator.prologue.len(), 14);
    }

    #[test]
    fn a_peer_speaking_something_else_is_refused() {
        let (mut a, mut b) = loopback();
        let other = std::thread::spawn(move || {
            b.write_all(b"HTTP/1.1 200 OK").unwrap();
            // Hold the endpoint open so the reader sees the bytes, not an end.
            std::thread::sleep(std::time::Duration::from_millis(50));
        });
        match negotiate(&mut a, Role::Initiator) {
            Err(VersionError::NotFerry(got)) => assert_ne!(got, MAGIC),
            other => panic!("expected a magic mismatch, got {other:?}"),
        }
        other.join().unwrap();
    }

    #[test]
    fn a_peer_offering_only_version_one_is_refused() {
        let (mut a, mut b) = loopback();
        let other = std::thread::spawn(move || {
            let mut sent = Vec::with_capacity(7);
            sent.extend_from_slice(&MAGIC);
            sent.extend_from_slice(&1u16.to_be_bytes());
            b.write_all(&sent).unwrap();
            // Hold the endpoint open so the reader sees the bytes, not an end.
            std::thread::sleep(std::time::Duration::from_millis(50));
        });
        match negotiate(&mut a, Role::Initiator) {
            Err(VersionError::NoSharedVersion { ours, theirs }) => {
                assert_eq!(ours, VERSION_MAX);
                assert_eq!(theirs, 1);
            }
            other => panic!("expected NoSharedVersion, got {other:?}"),
        }
        other.join().unwrap();
    }
}
