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
//! - `HANDSHAKE_TIMEOUT_SECS = 10`. One deadline for the whole handshake,
//!   meaning the version exchange and the Noise messages that follow it,
//!   however many reads or writes that takes. A plain socket timeout only
//!   bounds one read call, not a loop of them, so a private `DeadlineStream`
//!   enforces this instead.
//! - `MAX_PENDING_HANDSHAKES = 8`. A counter of accepted connections that have
//!   not finished a handshake. The ninth is closed at once.
//!
//! `Listener::accept` does only the TCP accept and the slot reservation. It
//! reads nothing, so a silent peer cannot make it wait, and cannot delay the
//! connections queued behind it. The version exchange and the Noise
//! handshake both happen later, inside `Pending::pair` or `Pending::connect`,
//! under the one deadline described above.
//!
//! After a handshake finishes, the read timeout is set to `IDLE_TIMEOUT_SECS`
//! instead of being cleared, so a peer that goes silent after pairing does
//! not hold its thread forever. The write timeout is cleared, since a write
//! only blocks when the peer stops reading, which is not covered here.
//!
//! Public shape:
//!
//! ```text
//! pub struct Listener { .. }
//! impl Listener {
//!     pub fn bind(addr: SocketAddr) -> io::Result<Self>;
//!     pub fn local_addr(&self) -> SocketAddr;
//!     /// Accept one connection and reserve a pending-handshake slot for it.
//!     /// Refuses when too many handshakes are already pending.
//!     pub fn accept(&self) -> Result<Pending, TcpError>;
//! }
//!
//! /// A connection that has been accepted, but has not yet agreed a version
//! /// or run a Noise handshake.
//! pub struct Pending { .. }
//! impl Pending {
//!     pub fn remote(&self) -> SocketAddr;
//!     /// Agree a version, then run Noise XX as responder, both inside one
//!     /// deadline. Starts the idle timeout on success.
//!     pub fn pair(self, key: &StaticKey) -> Result<PairedConnection, TcpError>;
//!     /// Agree a version, then run Noise KK as responder, trying each
//!     /// candidate key in turn against the one message the peer sends, and
//!     /// binding the first that authenticates. Both inside one deadline.
//!     /// Starts the idle timeout on success.
//!     pub fn connect(self, key: &StaticKey, candidates: &[PublicKey]) -> Result<Connection, TcpError>;
//! }
//!
//! pub struct Connection {
//!     pub stream: SecureStream,
//!     pub remote: SocketAddr,
//!     pub version: u16,
//!     /// Whichever candidate authenticated: the peer asked for, when this
//!     /// device dialled, or whichever of `candidates` did, when it accepted.
//!     pub peer: PublicKey,
//! }
//!
//! /// What a successful `pair` or `Pending::pair` produces. `Paired` has no
//! /// room for a version field and this crate does not own that type, so
//! /// this wraps it instead of changing it.
//! pub struct PairedConnection { pub paired: Paired, pub version: u16 }
//!
//! /// Dial a peer, agree a version, and run KK as initiator.
//! pub fn connect(addr: SocketAddr, key: &StaticKey, peer: &PublicKey) -> Result<Connection, TcpError>;
//! /// Dial a peer, agree a version, and run XX as initiator.
//! pub fn pair(addr: SocketAddr, key: &StaticKey) -> Result<PairedConnection, TcpError>;
//! ```
//!
//! `Paired` is `crate::noise::Paired`. The pending counter decrements when a
//! `Pending` is consumed or dropped, so a caller that abandons one does not
//! leak a slot.
//!
//! # Not yet
//!
//! A cap on how many connections one peer may hold at once belongs to a
//! server loop that does not exist yet in this crate. `MAX_PENDING_HANDSHAKES`
//! only limits handshakes in progress, not connections already served.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

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

/// How long a read may wait for data once a handshake has finished, in
/// seconds.
///
/// Five minutes is long enough that a person who pauses the app does not
/// lose the connection, and short enough that a peer that goes silent, or
/// dies, does not hold a thread and a socket open forever.
pub const IDLE_TIMEOUT_SECS: u64 = 300;

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

/// A `TcpStream` bounded by one deadline that shrinks with every call.
///
/// A plain socket timeout limits one call to `read` or `write`. Code such as
/// `read_exact` calls `read` in a loop. A peer that sends one byte per
/// timeout window resets the limit every time, and can hold the socket open
/// forever. This wrapper works out the time left before every call, and sets
/// the socket timeout to that. The whole sequence of calls is then bounded by
/// one deadline, no matter how many calls it takes.
///
/// `armed` is shared with the code that built this stream. The Noise
/// handshake keeps whatever stream it is given for the life of the
/// connection, so this wrapper cannot be swapped back out for a plain
/// `TcpStream` once the handshake has taken it. Turning `armed` off has the
/// same effect: later calls skip the deadline and go straight to the socket,
/// which is exactly what a plain `TcpStream` would do.
#[derive(Debug)]
struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
    armed: Arc<AtomicBool>,
}

impl DeadlineStream {
    /// The time left before the deadline.
    ///
    /// Returns a `TimedOut` error once the deadline has passed, rather than
    /// a zero duration, because a zero-length socket timeout is not a valid
    /// one.
    fn time_left(&self) -> io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the handshake did not finish before its deadline",
            ));
        }
        Ok(left)
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.armed.load(Ordering::SeqCst) {
            let left = self.time_left()?;
            self.stream.set_read_timeout(Some(left))?;
        }
        self.stream.read(buf)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.armed.load(Ordering::SeqCst) {
            let left = self.time_left()?;
            self.stream.set_write_timeout(Some(left))?;
        }
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Run the version exchange, then one Noise step, over `stream`. Both are
/// bounded by `deadline`, no matter how many reads or writes either one
/// takes.
///
/// `run` gets the agreed version alongside the stream, because it needs the
/// prologue from [`Agreed`] to start the Noise handshake. It moves the
/// stream into that handshake, and, on success, into the value it returns.
///
/// A cloned handle to the same socket is kept aside first. A timeout belongs
/// to the socket, not to whichever Rust value currently holds it, so setting
/// the idle timeout on that clone reaches the connection no matter which
/// wrapper the returned value keeps.
fn run_handshake<T>(
    stream: TcpStream,
    role: Role,
    deadline: Instant,
    idle_timeout: Duration,
    run: impl FnOnce(DeadlineStream, Agreed) -> Result<T, NoiseError>,
) -> Result<T, TcpError> {
    let idle_handle = stream.try_clone()?;
    let armed = Arc::new(AtomicBool::new(true));
    let mut deadline_stream = DeadlineStream {
        stream,
        deadline,
        armed: Arc::clone(&armed),
    };

    let agreed = map_version(version::negotiate(&mut deadline_stream, role))?;
    let value = map_noise(run(deadline_stream, agreed))?;

    // The handshake is done. Turn the deadline off, and start the idle
    // timeout instead of clearing it, so a peer that later goes silent does
    // not hold this thread forever.
    armed.store(false, Ordering::SeqCst);
    idle_handle.set_read_timeout(Some(idle_timeout))?;
    idle_handle.set_write_timeout(None)?;

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

/// Accepts TCP connections and reserves a handshake slot for each one.
#[derive(Debug)]
pub struct Listener {
    inner: TcpListener,
    local_addr: SocketAddr,
    pending: Arc<AtomicU32>,
    handshake_timeout: Duration,
    idle_timeout: Duration,
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
            idle_timeout: Duration::from_secs(IDLE_TIMEOUT_SECS),
        })
    }

    /// Use `idle_timeout` instead of [`IDLE_TIMEOUT_SECS`] for connections
    /// this listener hands out.
    ///
    /// Production code never calls this. It exists so a test can wait out a
    /// short idle timeout instead of the real one.
    #[cfg(test)]
    #[must_use]
    fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
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

    /// Accept one connection and reserve a pending-handshake slot for it.
    ///
    /// This does no I/O beyond the TCP accept itself. The version exchange
    /// and the Noise handshake happen later, inside [`Pending::pair`] or
    /// [`Pending::connect`], so a peer that never sends a byte cannot make
    /// `accept` wait, and cannot delay the connections queued behind it.
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
    /// reached.
    pub fn accept(&self) -> Result<Pending, TcpError> {
        let (stream, remote) = self.inner.accept()?;
        // Every frame and Noise message is its own write. Without this, the
        // second write of a pair waits on a delayed acknowledgement from the
        // other side, which costs tens of milliseconds per request. A
        // failure to set it does not stop the connection from working, so
        // the error is ignored rather than failing the accept.
        let _ = stream.set_nodelay(true);
        let slot = self.reserve_slot()?;
        let deadline = Instant::now() + self.handshake_timeout;

        Ok(Pending {
            stream,
            remote,
            slot,
            deadline,
            idle_timeout: self.idle_timeout,
        })
    }
}

/// A connection that has been accepted, but has not yet agreed a version or
/// run a Noise handshake.
///
/// It holds one pending-handshake slot and the deadline its handshake must
/// finish by. [`Pending::pair`] and [`Pending::connect`] each run the version
/// exchange and the Noise handshake together, bounded by that one deadline.
/// The slot is freed when either finishes, or when this value is dropped
/// without calling either.
#[derive(Debug)]
pub struct Pending {
    stream: TcpStream,
    remote: SocketAddr,
    slot: PendingSlot,
    deadline: Instant,
    idle_timeout: Duration,
}

impl Pending {
    /// The address of the peer that connected.
    #[must_use]
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }

    /// Agree a version, then run Noise XX as responder, both inside one
    /// deadline. Starts the idle timeout on success.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Version`] when version negotiation fails,
    /// [`TcpError::Noise`] when the handshake fails, and
    /// [`TcpError::Timeout`] when the two together do not finish before the
    /// deadline.
    pub fn pair(self, key: &StaticKey) -> Result<PairedConnection, TcpError> {
        // Destructuring keeps `slot` alive, under its own name, until this
        // function returns. Its `Drop` then frees the slot exactly once,
        // whether the handshake below succeeds or fails.
        let Pending {
            stream,
            deadline,
            idle_timeout,
            slot: _slot,
            ..
        } = self;
        run_handshake(
            stream,
            Role::Responder,
            deadline,
            idle_timeout,
            |s, agreed| {
                noise::pair_as_responder(s, key, &agreed.prologue).map(|paired| PairedConnection {
                    paired,
                    version: agreed.version,
                })
            },
        )
    }

    /// Agree a version, then run Noise KK as responder, trying each of
    /// `candidates` in turn against the one message the peer sends, and
    /// binding the first that authenticates, both inside one deadline.
    /// Starts the idle timeout on success.
    ///
    /// The wire says nothing about who is calling before the handshake, and
    /// message one can only be read once, so every candidate the caller
    /// might be is tried against that same read. `docs/engine-contract.md`
    /// item 16b.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Version`] when version negotiation fails,
    /// [`TcpError::Noise`] when no candidate authenticates, and
    /// [`TcpError::Timeout`] when the two together do not finish before the
    /// deadline.
    pub fn connect(
        self,
        key: &StaticKey,
        candidates: &[PublicKey],
    ) -> Result<Connection, TcpError> {
        let Pending {
            stream,
            remote,
            deadline,
            idle_timeout,
            slot: _slot,
        } = self;
        run_handshake(
            stream,
            Role::Responder,
            deadline,
            idle_timeout,
            |s, agreed| {
                noise::connect_as_responder_any(s, key, candidates, &agreed.prologue).map(
                    |(stream, peer)| Connection {
                        stream,
                        remote,
                        version: agreed.version,
                        peer,
                    },
                )
            },
        )
    }
}

/// What a successful [`connect`] or [`Pending::connect`] produces.
#[derive(Debug)]
pub struct Connection {
    /// The encrypted channel, ready to carry frames.
    pub stream: SecureStream,
    /// The address of the peer.
    pub remote: SocketAddr,
    /// The protocol version both sides agreed on, before the Noise handshake
    /// ran.
    pub version: u16,
    /// The peer this connection authenticated as. For [`connect`], the peer
    /// the caller asked for. For [`Pending::connect`], whichever of its
    /// candidates actually authenticated. `docs/engine-contract.md` item
    /// 16b.
    pub peer: PublicKey,
}

/// What a successful [`pair`] or [`Pending::pair`] produces.
///
/// [`Paired`], from `crate::noise`, has no room for a version field, and this
/// crate does not own that type, so this wraps it instead of changing it.
#[derive(Debug)]
pub struct PairedConnection {
    /// The encrypted channel and the pairing code, exactly as Noise XX
    /// produced them.
    pub paired: Paired,
    /// The protocol version both sides agreed on, before pairing began.
    pub version: u16,
}

/// Dial a peer, agree a version, and run KK as initiator.
///
/// # Errors
///
/// Returns [`TcpError::Version`] when version negotiation fails,
/// [`TcpError::Noise`] when the peer does not hold the private key matching
/// `peer`, and [`TcpError::Timeout`] when the two together do not finish
/// before the handshake timeout.
pub fn connect(
    addr: SocketAddr,
    key: &StaticKey,
    peer: &PublicKey,
) -> Result<Connection, TcpError> {
    let stream = TcpStream::connect(addr)?;
    // As in `Listener::accept`: without this, a small write waits on a
    // delayed acknowledgement instead of reaching the wire at once.
    let _ = stream.set_nodelay(true);
    let remote = stream.peer_addr()?;
    let deadline = Instant::now() + Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS);
    run_handshake(
        stream,
        Role::Initiator,
        deadline,
        Duration::from_secs(IDLE_TIMEOUT_SECS),
        |s, agreed| {
            noise::connect_as_initiator(s, key, peer, &agreed.prologue).map(|stream| Connection {
                stream,
                remote,
                version: agreed.version,
                peer: *peer,
            })
        },
    )
}

/// Dial a peer, agree a version, and run XX as initiator.
///
/// # Errors
///
/// As [`connect`], except there is no stored peer key to check, so a
/// [`TcpError::Noise`] here means the handshake itself failed rather than
/// that the peer's identity was wrong.
pub fn pair(addr: SocketAddr, key: &StaticKey) -> Result<PairedConnection, TcpError> {
    let stream = TcpStream::connect(addr)?;
    // As in `Listener::accept`: without this, a small write waits on a
    // delayed acknowledgement instead of reaching the wire at once.
    let _ = stream.set_nodelay(true);
    let deadline = Instant::now() + Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS);
    run_handshake(
        stream,
        Role::Initiator,
        deadline,
        Duration::from_secs(IDLE_TIMEOUT_SECS),
        |s, agreed| {
            noise::pair_as_initiator(s, key, &agreed.prologue).map(|paired| PairedConnection {
                paired,
                version: agreed.version,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{Listener, Pending, TcpError, connect, pair};
    use crate::limits;
    use crate::noise::StaticKey;
    use crate::version::{MAGIC, VERSION_MAX};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

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

        assert_eq!(initiator.paired.code, responder.paired.code);
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
            let mut connection = pending.connect(&key_b, &[public_a]).unwrap();
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

        // Each of these clients connects, so `accept` returns a `Pending`
        // for it right away; `accept` no longer needs a version exchange to
        // finish first. None of them goes on to pair or connect, so each
        // slot stays reserved until the test drops the `Pending` values
        // below.
        let clients: Vec<_> = (0..limits::MAX_PENDING_HANDSHAKES)
            .map(|_| {
                thread::spawn(move || {
                    let mut stream = TcpStream::connect(addr).unwrap();
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

        // `accept` itself no longer waits on the handshake, so the timeout
        // now shows up once the handshake actually runs, inside `pair`.
        let pending = listener.accept().unwrap();
        let key = StaticKey::generate().unwrap();
        match pending.pair(&key) {
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
            pending.connect(&key_server, &[public_client]).map(|_| ())
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

    #[test]
    fn a_client_that_drips_one_byte_per_window_is_still_dropped_at_the_deadline() {
        let listener =
            Listener::bind_with_timeout(local_any(), Duration::from_millis(300)).unwrap();
        let addr = listener.local_addr();

        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();

            // The 7 byte version exchange, dripped one byte every 200ms. A
            // socket timeout that resets on every read would let this alone
            // take well over a second.
            let mut version_bytes = Vec::with_capacity(7);
            version_bytes.extend_from_slice(&MAGIC);
            version_bytes.extend_from_slice(&VERSION_MAX.to_be_bytes());
            for byte in version_bytes {
                if stream.write_all(&[byte]).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }

            // A valid-looking handshake length prefix, then a body dripped
            // the same way. The body never fully arrives.
            if stream.write_all(&900u16.to_be_bytes()).is_err() {
                return;
            }
            loop {
                if stream.write_all(&[0u8]).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }
        });

        let key = StaticKey::generate().unwrap();
        let pending = listener.accept().unwrap();

        let start = Instant::now();
        let result = pending.connect(&key, &[key.public()]);
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(TcpError::Timeout)),
            "expected a timeout, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(450),
            "expected the deadline to cut the handshake off quickly, took {elapsed:?}"
        );

        client.join().unwrap();
    }

    #[test]
    fn a_silent_client_does_not_delay_the_next_accept() {
        let listener =
            Listener::bind_with_timeout(local_any(), Duration::from_millis(500)).unwrap();
        let addr = listener.local_addr();

        // A silent client. It connects but never sends a byte, so it must
        // not make `accept`, or anyone else's handshake, wait for it.
        let _silent = TcpStream::connect(addr).unwrap();

        let key_initiator = StaticKey::generate().unwrap();
        let start = Instant::now();
        let second_client = thread::spawn(move || pair(addr, &key_initiator).unwrap());

        // Two accepts, one per connection. Neither does any I/O, so both
        // return right away, before either handshake has run.
        let silent_pending = listener.accept().unwrap();
        let second_pending = listener.accept().unwrap();

        // Each `Pending` runs its handshake on its own thread, as a real
        // server would. The silent one is left to time out on its own.
        let silent = thread::spawn(move || {
            let key = StaticKey::generate().unwrap();
            silent_pending.pair(&key)
        });
        let key_responder = StaticKey::generate().unwrap();
        let responder = thread::spawn(move || second_pending.pair(&key_responder).unwrap());

        let responder_result = responder.join().unwrap();
        let elapsed = start.elapsed();
        let initiator_result = second_client.join().unwrap();

        assert_eq!(initiator_result.paired.code, responder_result.paired.code);
        assert!(
            elapsed < Duration::from_millis(500),
            "the second handshake should not wait on the silent client, took {elapsed:?}"
        );

        let _ = silent.join();
    }

    #[test]
    fn an_idle_connection_read_times_out() {
        let listener = Listener::bind_with_timeout(local_any(), Duration::from_secs(5))
            .unwrap()
            .with_idle_timeout(Duration::from_millis(200));
        let addr = listener.local_addr();

        let key_responder = StaticKey::generate().unwrap();
        let server = thread::spawn(move || {
            let pending = listener.accept().unwrap();
            let mut connection = pending.pair(&key_responder).unwrap();
            let start = Instant::now();
            let mut buf = [0u8; 1];
            let result = connection.paired.stream.read(&mut buf);
            (result, start.elapsed())
        });

        let key_initiator = StaticKey::generate().unwrap();
        // Kept alive until the server side has read from it, so the read
        // times out on idleness, not on a closed connection.
        let client = pair(addr, &key_initiator).unwrap();

        let (result, elapsed) = server.join().unwrap();

        assert!(result.is_err(), "expected the idle read to time out");
        assert!(
            elapsed < Duration::from_millis(350),
            "expected the idle timeout to cut the read off quickly, took {elapsed:?}"
        );

        drop(client);
    }
}
