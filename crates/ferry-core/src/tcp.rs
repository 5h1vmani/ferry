//! The network transport: TCP, with the handshake limits enforced.
//!
//! Implemented below. The contract that follows states what this module does.
//!
//! # Contract
//!
//! This module owns sockets and timeouts. It does not own cryptography. It
//! hands a stream to `crate::noise` once the version exchange is done.
//!
//! Two limits from `docs/protocol.md` section 7 that were "designed but not
//! built" become real here, and their constants return to `crate::limits`:
//!
//! - `HANDSHAKE_TIMEOUT_SECS = 10`. Set as the socket read and write timeout
//!   until the handshake completes, then cleared.
//! - `MAX_PENDING_HANDSHAKES = 8`. A counter of accepted connections that have
//!   not finished a handshake. The ninth is closed at once.
//!
//! Public shape:
//!
//! ```text
//! pub struct Listener { .. }
//! impl Listener {
//!     pub fn bind(addr: SocketAddr) -> io::Result<Self>;
//!     pub fn local_addr(&self) -> SocketAddr;
//!     /// Accept one connection and run the version exchange under the
//!     /// timeout. Refuses when too many handshakes are pending.
//!     pub fn accept(&self) -> Result<Pending, TcpError>;
//! }
//!
//! /// A connection that has agreed a version and nothing else yet.
//! pub struct Pending { .. }
//! impl Pending {
//!     pub fn remote(&self) -> SocketAddr;
//!     pub fn version(&self) -> u16;
//!     /// Run Noise XX as responder. Clears the timeout on success.
//!     pub fn pair(self, key: &StaticKey) -> Result<Paired, TcpError>;
//!     /// Run Noise KK as responder against a known peer. Clears the timeout.
//!     pub fn connect(self, key: &StaticKey, peer: &PublicKey) -> Result<Connection, TcpError>;
//! }
//!
//! pub struct Connection { pub stream: SecureStream, pub remote: SocketAddr }
//!
//! /// Dial a peer, agree a version, and run KK as initiator.
//! pub fn connect(addr: SocketAddr, key: &StaticKey, peer: &PublicKey) -> Result<Connection, TcpError>;
//! /// Dial a peer, agree a version, and run XX as initiator.
//! pub fn pair(addr: SocketAddr, key: &StaticKey) -> Result<Paired, TcpError>;
//! ```
//!
//! `Paired` is `crate::noise::Paired`. The pending counter decrements when a
//! `Pending` is consumed or dropped, so a caller that abandons one does not
//! leak a slot.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::limits;
use crate::noise::{self, NoiseError, Paired, PublicKey, SecureStream, StaticKey};
use crate::version::{self, Agreed, Role, VersionError};

/// The reason a TCP connection did not become a usable channel.
#[derive(Debug, thiserror::Error)]
pub enum TcpError {
    /// The stream failed.
    #[error("stream failed: {0}")]
    Io(#[from] io::Error),
    /// Version negotiation failed.
    #[error("version negotiation failed: {0}")]
    Version(#[from] VersionError),
    /// The Noise handshake failed.
    #[error("the Noise handshake failed: {0}")]
    Noise(#[from] NoiseError),
    /// The listener already holds [`limits::MAX_PENDING_HANDSHAKES`]
    /// connections that have not finished a handshake. This one is refused
    /// at once, before anything is read from it.
    #[error(
        "too many handshakes are already pending, the limit is {}",
        limits::MAX_PENDING_HANDSHAKES
    )]
    TooManyPending,
    /// The handshake did not finish before the timeout.
    ///
    /// A read or a write on the socket ran past the deadline. Either side of
    /// the handshake can time out this way, so the caller sees one clear
    /// answer instead of a raw I/O error.
    #[error("the handshake did not finish before the timeout")]
    Timeout,
}

/// True when an I/O error is a read or a write that ran past its deadline.
///
/// The two kinds exist because platforms differ in which one a timed-out
/// socket call reports.
fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Turn a version-negotiation failure into a `TcpError`, folding a timed-out
/// read or write into `TcpError::Timeout` so callers get one clear answer.
fn map_version(result: Result<Agreed, VersionError>) -> Result<Agreed, TcpError> {
    result.map_err(|error| match &error {
        VersionError::Io(io_error) if is_timeout(io_error) => TcpError::Timeout,
        _ => TcpError::Version(error),
    })
}

/// As `map_version`, for a Noise handshake failure.
fn map_noise<T>(result: Result<T, NoiseError>) -> Result<T, TcpError> {
    result.map_err(|error| match &error {
        NoiseError::Io(io_error) if is_timeout(io_error) => TcpError::Timeout,
        _ => TcpError::Noise(error),
    })
}

/// Set the read and write timeout to `timeout` on both directions of
/// `stream`.
fn set_timeouts(stream: &TcpStream, timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(())
}

/// Clear the read and write timeout on both directions of `stream`.
fn clear_timeouts(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok(())
}

/// Run one Noise step over `stream`, then clear its handshake timeout.
///
/// `run` moves `stream` into the Noise handshake and, on success, into the
/// value it returns, so this keeps a cloned handle to the same socket to
/// clear the timeout afterwards. A clone shares the underlying socket, so
/// the timeout it sets or clears applies to the original handle too.
fn finish_handshake<T>(
    stream: TcpStream,
    run: impl FnOnce(TcpStream) -> Result<T, NoiseError>,
) -> Result<T, TcpError> {
    let timeout_handle = stream.try_clone()?;
    let value = map_noise(run(stream))?;
    clear_timeouts(&timeout_handle)?;
    Ok(value)
}

/// Reserves one pending-handshake slot, and gives it back when dropped.
///
/// This is the only place the counter changes. A slot can only be created
/// while one is free, and dropping it is the only way to free one again, so
/// the count can never be missed or double counted.
#[derive(Debug)]
struct PendingSlot(Arc<AtomicU32>);

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accepts TCP connections and runs the version exchange on each one.
#[derive(Debug)]
pub struct Listener {
    inner: TcpListener,
    local_addr: SocketAddr,
    pending: Arc<AtomicU32>,
    handshake_timeout: Duration,
}

impl Listener {
    /// Bind to `addr` and accept connections.
    ///
    /// # Errors
    ///
    /// Returns an error when the address cannot be bound.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        Self::bind_with_timeout(addr, Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS))
    }

    /// Bind to `addr`, using `handshake_timeout` instead of
    /// [`limits::HANDSHAKE_TIMEOUT_SECS`].
    ///
    /// Production code should call [`Listener::bind`]. This exists so a test
    /// can use a short timeout instead of waiting out the real one.
    ///
    /// # Errors
    ///
    /// Returns an error when the address cannot be bound.
    pub fn bind_with_timeout(addr: SocketAddr, handshake_timeout: Duration) -> io::Result<Self> {
        let inner = TcpListener::bind(addr)?;
        let local_addr = inner.local_addr()?;
        Ok(Self {
            inner,
            local_addr,
            pending: Arc::new(AtomicU32::new(0)),
            handshake_timeout,
        })
    }

    /// The address this listener is bound to.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Reserve a pending-handshake slot, or refuse when
    /// [`limits::MAX_PENDING_HANDSHAKES`] are already reserved.
    fn reserve_slot(&self) -> Result<PendingSlot, TcpError> {
        self.pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current < limits::MAX_PENDING_HANDSHAKES).then_some(current + 1)
            })
            .map_err(|_| TcpError::TooManyPending)?;
        Ok(PendingSlot(Arc::clone(&self.pending)))
    }

    /// Accept one connection and run the version exchange under the
    /// handshake timeout.
    ///
    /// The pending-handshake limit is checked as soon as the connection is
    /// accepted, before anything is read from it, so a ninth connection is
    /// refused at once instead of waiting behind the other eight. A
    /// connection counts as pending from here until the returned `Pending`
    /// is consumed by [`Pending::pair`] or [`Pending::connect`], or dropped.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::TooManyPending`] when the limit above is already
    /// reached, and [`TcpError::Timeout`] when the version exchange does not
    /// finish before the handshake timeout.
    pub fn accept(&self) -> Result<Pending, TcpError> {
        let (mut stream, remote) = self.inner.accept()?;
        let slot = self.reserve_slot()?;

        set_timeouts(&stream, self.handshake_timeout)?;
        let agreed = map_version(version::negotiate(&mut stream, Role::Responder))?;

        Ok(Pending {
            stream,
            remote,
            agreed,
            slot,
        })
    }
}

/// A connection that has agreed a version and nothing else yet.
///
/// It holds one pending-handshake slot. The slot is freed when [`Pending::pair`]
/// or [`Pending::connect`] finishes, or when this value is dropped without
/// calling either.
#[derive(Debug)]
pub struct Pending {
    stream: TcpStream,
    remote: SocketAddr,
    agreed: Agreed,
    slot: PendingSlot,
}

impl Pending {
    /// The address of the peer that connected.
    #[must_use]
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }

    /// The protocol version both sides agreed on.
    #[must_use]
    pub fn version(&self) -> u16 {
        self.agreed.version
    }

    /// Run Noise XX as responder. Clears the handshake timeout on success.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Noise`] when the handshake fails, and
    /// [`TcpError::Timeout`] when it does not finish before the timeout.
    pub fn pair(self, key: &StaticKey) -> Result<Paired, TcpError> {
        // Destructuring keeps `slot` alive, under its own name, until this
        // function returns. Its `Drop` then frees the slot exactly once,
        // whether the handshake below succeeds or fails.
        let Pending {
            stream,
            agreed,
            slot: _slot,
            ..
        } = self;
        finish_handshake(stream, |s| {
            noise::pair_as_responder(s, key, &agreed.prologue)
        })
    }

    /// Run Noise KK as responder against a known peer. Clears the handshake
    /// timeout on success.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Noise`] when the peer does not hold the private
    /// key matching `peer`, and [`TcpError::Timeout`] when the handshake
    /// does not finish before the timeout.
    pub fn connect(self, key: &StaticKey, peer: &PublicKey) -> Result<Connection, TcpError> {
        let Pending {
            stream,
            remote,
            agreed,
            slot: _slot,
        } = self;
        let stream = finish_handshake(stream, |s| {
            noise::connect_as_responder(s, key, peer, &agreed.prologue)
        })?;
        Ok(Connection { stream, remote })
    }
}

/// What a successful [`connect`] or [`Pending::connect`] produces.
#[derive(Debug)]
pub struct Connection {
    /// The encrypted channel, ready to carry frames.
    pub stream: SecureStream,
    /// The address of the peer.
    pub remote: SocketAddr,
}

/// Dial a peer, agree a version, and run KK as initiator.
///
/// # Errors
///
/// Returns [`TcpError::Version`] when version negotiation fails,
/// [`TcpError::Noise`] when the peer does not hold the private key matching
/// `peer`, and [`TcpError::Timeout`] when either step does not finish before
/// the handshake timeout.
pub fn connect(
    addr: SocketAddr,
    key: &StaticKey,
    peer: &PublicKey,
) -> Result<Connection, TcpError> {
    let mut stream = TcpStream::connect(addr)?;
    let remote = stream.peer_addr()?;
    set_timeouts(&stream, Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS))?;
    let agreed = map_version(version::negotiate(&mut stream, Role::Initiator))?;
    let stream = finish_handshake(stream, |s| {
        noise::connect_as_initiator(s, key, peer, &agreed.prologue)
    })?;
    Ok(Connection { stream, remote })
}

/// Dial a peer, agree a version, and run XX as initiator.
///
/// # Errors
///
/// As [`connect`], except there is no stored peer key to check, so a
/// [`TcpError::Noise`] here means the handshake itself failed rather than
/// that the peer's identity was wrong.
pub fn pair(addr: SocketAddr, key: &StaticKey) -> Result<Paired, TcpError> {
    let mut stream = TcpStream::connect(addr)?;
    set_timeouts(&stream, Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS))?;
    let agreed = map_version(version::negotiate(&mut stream, Role::Initiator))?;
    finish_handshake(stream, |s| {
        noise::pair_as_initiator(s, key, &agreed.prologue)
    })
}

#[cfg(test)]
mod tests {
    use super::{Listener, Pending, TcpError, connect, pair};
    use crate::limits;
    use crate::noise::StaticKey;
    use crate::version::{self, Role};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::thread;
    use std::time::Duration;

    /// A loopback address with no fixed port, so tests never collide.
    fn local_any() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    #[test]
    fn a_pairing_over_tcp_agrees_the_same_code_on_both_sides() {
        let listener = Listener::bind(local_any()).unwrap();
        let addr = listener.local_addr();

        let key_responder = StaticKey::generate().unwrap();
        let server = thread::spawn(move || {
            let pending = listener.accept().unwrap();
            pending.pair(&key_responder).unwrap()
        });

        let key_initiator = StaticKey::generate().unwrap();
        let initiator = pair(addr, &key_initiator).unwrap();
        let responder = server.join().unwrap();

        assert_eq!(initiator.code, responder.code);
    }

    #[test]
    fn a_connect_over_tcp_after_a_stored_pairing_carries_bytes_both_ways() {
        let key_a = StaticKey::generate().unwrap();
        let key_b = StaticKey::generate().unwrap();
        let (public_a, public_b) = (key_a.public(), key_b.public());

        let listener = Listener::bind(local_any()).unwrap();
        let addr = listener.local_addr();

        let server = thread::spawn(move || {
            let pending = listener.accept().unwrap();
            let mut connection = pending.connect(&key_b, &public_a).unwrap();
            let mut buf = [0u8; 5];
            connection.stream.read_exact(&mut buf).unwrap();
            connection.stream.write_all(b"world").unwrap();
            buf
        });

        let mut connection = connect(addr, &key_a, &public_b).unwrap();
        connection.stream.write_all(b"hello").unwrap();
        let mut buf = [0u8; 5];
        connection.stream.read_exact(&mut buf).unwrap();

        assert_eq!(&buf, b"world");
        assert_eq!(&server.join().unwrap(), b"hello");
    }

    #[test]
    fn the_ninth_pending_handshake_is_refused() {
        let listener = Listener::bind(local_any()).unwrap();
        let addr = listener.local_addr();

        // Each of these clients completes the version exchange, so `accept`
        // returns a `Pending` for it right away. None of them goes on to
        // pair or connect, so each slot stays reserved until the test drops
        // the `Pending` values below.
        let clients: Vec<_> = (0..limits::MAX_PENDING_HANDSHAKES)
            .map(|_| {
                thread::spawn(move || {
                    let mut stream = TcpStream::connect(addr).unwrap();
                    version::negotiate(&mut stream, Role::Initiator).unwrap();
                    // Block here until the server side closes, so the socket
                    // stays open for as long as the test needs it pending.
                    let mut buf = [0u8; 1];
                    let _ = stream.read(&mut buf);
                })
            })
            .collect();

        let mut pending: Vec<Pending> = Vec::new();
        for _ in 0..limits::MAX_PENDING_HANDSHAKES {
            pending.push(listener.accept().unwrap());
        }

        // A ninth client reaches the TCP-level accept but never negotiates.
        // The limit is checked before the listener would block waiting for
        // it, so this does not hang.
        let ninth = TcpStream::connect(addr).unwrap();
        assert!(matches!(listener.accept(), Err(TcpError::TooManyPending)));

        drop(ninth);
        // Dropping the held `Pending` values frees their slots and closes
        // their sockets, which lets each client thread's blocked read end.
        drop(pending);
        for client in clients {
            client.join().unwrap();
        }
    }

    #[test]
    fn a_client_that_sends_nothing_is_dropped_after_the_timeout() {
        let listener =
            Listener::bind_with_timeout(local_any(), Duration::from_millis(200)).unwrap();
        let addr = listener.local_addr();

        let _client = TcpStream::connect(addr).unwrap();

        match listener.accept() {
            Err(TcpError::Timeout) => {}
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_wrong_key_client_is_refused_on_connect() {
        let key_server = StaticKey::generate().unwrap();
        let key_client = StaticKey::generate().unwrap();
        let stranger = StaticKey::generate().unwrap();
        let public_server = key_server.public();
        let public_client = key_client.public();

        let listener = Listener::bind(local_any()).unwrap();
        let addr = listener.local_addr();

        let server = thread::spawn(move || {
            let pending = listener.accept().unwrap();
            pending.connect(&key_server, &public_client).map(|_| ())
        });

        // The stranger dials in, but does not hold the private key the
        // server expects to see from `public_client`.
        let attempt = connect(addr, &stranger, &public_server);
        let server_result = server.join().unwrap();

        assert!(
            attempt.is_err() || server_result.is_err(),
            "a client without the paired key must be refused"
        );
    }
}
