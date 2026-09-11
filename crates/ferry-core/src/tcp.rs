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
//! Audit `docs/audits/fable-security.md`, findings 1 and 4, add two more:
//!
//! - `FIRST_BYTE_TIMEOUT_SECS = 2`. Checked in [`Pending::negotiate`], before
//!   the version exchange starts: a connection that has not sent one byte
//!   within two seconds of being accepted is dropped, instead of holding its
//!   pending slot for the whole ten second [`HANDSHAKE_TIMEOUT_SECS`].
//! - `MAX_PENDING_HANDSHAKES_PER_ADDR = 2`. `Listener` also counts pending
//!   handshakes by the connecting `IpAddr`, so one address opening
//!   connections and sending nothing cannot use up every slot
//!   `MAX_PENDING_HANDSHAKES` allows and starve every other address queued
//!   behind it. The overall cap is unchanged and still applies on top of
//!   this one.
//!
//! Either refusal drops the socket at once and reports nothing: a peer that
//! cannot even get a pending slot learns nothing more by being told so.
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
//! `run_handshake` also hands back a clone of the raw socket, taken before
//! the handshake boxes the stream inside a `SecureStream`. `Connection`
//! carries it as `socket`, for the caller to register with `Shared`, so
//! `stop` can call `shutdown` on it directly instead of waiting out
//! `IDLE_TIMEOUT_SECS` for a peer that stopped answering.
//! `docs/engine-contract.md` item 16c.
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
//!     /// Agree a version and a mode, inside the deadline. The mode names
//!     /// which Noise pattern the initiator is about to run; the caller
//!     /// reads it before choosing which of `NegotiatedPending`'s three
//!     /// finishers to call, since a handshake state has to be built for
//!     /// one exact pattern before a single byte of it can be read.
//!     pub fn negotiate(self) -> Result<NegotiatedPending, TcpError>;
//! }
//!
//! /// A `Pending` whose version and mode are known. Exactly one of the three
//! /// methods below is the right one to call, chosen by `mode()`.
//! pub struct NegotiatedPending { .. }
//! impl NegotiatedPending {
//!     pub fn remote(&self) -> SocketAddr;
//!     pub fn mode(&self) -> Mode;
//!     /// Run Noise XX as responder, inside the same deadline. Starts the
//!     /// idle timeout on success.
//!     pub fn pair(self, key: &StaticKey) -> Result<PairedConnection, TcpError>;
//!     /// Run Noise IK as responder, inside the same deadline. Starts the
//!     /// idle timeout on success.
//!     pub fn pair_ik(self, key: &StaticKey, expected_nonce: Option<&[u8; 16]>) -> Result<IkPairedConnection, TcpError>;
//!     /// Run Noise KK as responder, trying each candidate key in turn
//!     /// against the one message the peer sends, and binding the first
//!     /// that authenticates. Inside the same deadline. Starts the idle
//!     /// timeout on success.
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
//!     /// A clone of the raw socket, taken before `stream` boxed it. The
//!     /// caller registers this with `Shared` so `stop` can close it
//!     /// directly, both dialled and accepted.
//!     pub socket: TcpStream,
//! }
//!
//! /// What a successful `pair` or `NegotiatedPending::pair` produces.
//! /// `Paired` has no room for a version field and this crate does not own
//! /// that type, so this wraps it instead of changing it.
//! pub struct PairedConnection { pub paired: Paired, pub version: u16 }
//!
//! /// What a successful `pair_ik` or `NegotiatedPending::pair_ik` produces.
//! /// `IkAccepted` already carries everything a QR pairing needs and no
//! /// code, so this only adds the version, for symmetry with the other two
//! /// connection types.
//! pub struct IkPairedConnection { pub accepted: IkAccepted, pub version: u16 }
//!
//! /// Dial a peer, agree a version, and run KK as initiator.
//! pub fn connect(addr: SocketAddr, key: &StaticKey, peer: &PublicKey) -> Result<Connection, TcpError>;
//! /// Dial a peer, agree a version, and run XX as initiator.
//! pub fn pair(addr: SocketAddr, key: &StaticKey) -> Result<PairedConnection, TcpError>;
//! /// Dial the address from a scanned QR offer, agree a version, and run IK
//! /// as initiator, with the offer's nonce and this device's hello in
//! /// message one.
//! pub fn pair_ik(addr: SocketAddr, key: &StaticKey, responder: &PublicKey, nonce: &[u8; 16], my_name: &str, my_kind: DeviceKind) -> Result<SecureStream, TcpError>;
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

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::limits;
use crate::noise::{self, IkAccepted, NoiseError, Paired, PublicKey, SecureStream, StaticKey};
use crate::peers::DeviceKind;
use crate::version::{self, Agreed, Mode, Role, VersionError};

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
    /// The listener already holds
    /// [`limits::MAX_PENDING_HANDSHAKES_PER_ADDR`] connections from this
    /// same source address that have not finished a handshake. Refused at
    /// once, the same as [`TcpError::TooManyPending`], so one address
    /// cannot hold every pending slot by itself.
    #[error(
        "too many handshakes are already pending from this address, the limit is {}",
        limits::MAX_PENDING_HANDSHAKES_PER_ADDR
    )]
    TooManyPendingFromAddr,
    /// The handshake did not finish before the timeout.
    ///
    /// A read or a write on the socket ran past the deadline. Either side of
    /// the handshake can time out this way, so the caller sees one clear
    /// answer instead of a raw I/O error. Also returned when a connection
    /// sends no byte at all within [`limits::FIRST_BYTE_TIMEOUT_SECS`], the
    /// shorter deadline [`Pending::negotiate`] checks first.
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

/// Wait for `stream` to have at least one byte ready, without consuming it,
/// inside [`limits::FIRST_BYTE_TIMEOUT_SECS`].
///
/// Audit `docs/audits/fable-security.md`, findings 1 and 4: a connection
/// that is accepted and then sends nothing used to hold its pending slot
/// for the whole [`limits::HANDSHAKE_TIMEOUT_SECS`], eight seconds longer
/// than it needed to. This runs first, before [`begin_handshake`] starts
/// the version exchange, so a silent connection is refused in two seconds
/// instead, and frees its slot that much sooner for whoever is queued
/// behind it. `peek` rather than `read`, so the byte is still there for the
/// version exchange to read for real once this returns.
///
/// `handshake_deadline` is [`Pending`]'s own overall deadline. The shorter
/// of it and [`limits::FIRST_BYTE_TIMEOUT_SECS`] from now wins, so a test
/// that binds with a short custom handshake timeout (see
/// [`Listener::bind_with_timeout`]) still gets a short wait here too,
/// instead of always waiting out the full two real seconds.
fn wait_for_first_byte(stream: &TcpStream, handshake_deadline: Instant) -> Result<(), TcpError> {
    let deadline = handshake_deadline.min(Instant::now() + Duration::from_secs(limits::FIRST_BYTE_TIMEOUT_SECS));
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(TcpError::Timeout);
    }
    stream.set_read_timeout(Some(left))?;
    let mut byte = [0u8; 1];
    match stream.peek(&mut byte) {
        // The peer closed the connection without sending anything.
        Ok(0) => Err(TcpError::Timeout),
        Ok(_) => Ok(()),
        Err(error) if is_timeout(&error) => Err(TcpError::Timeout),
        Err(error) => Err(TcpError::Io(error)),
    }
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

/// The version exchange, half done: the deadline-bounded stream and the two
/// socket clones `run_handshake` used to keep aside are held here instead,
/// so a caller that needs to read [`Agreed::mode`] before it can pick a
/// Noise pattern is able to, without negotiating twice.
///
/// See [`begin_handshake`] to build one and [`Negotiating::negotiate`] and
/// [`Negotiating::finish`] to drive it. [`run_handshake`] is the same three
/// steps glued together, for every caller that already knows its pattern
/// before it starts.
#[derive(Debug)]
struct Negotiating {
    deadline_stream: DeadlineStream,
    idle_handle: TcpStream,
    registered_socket: TcpStream,
    armed: Arc<AtomicBool>,
    idle_timeout: Duration,
}

/// Start a handshake over `stream`, bounded by `deadline`. Does no I/O.
///
/// Two clones of the same socket are kept aside first, before `stream` is
/// wrapped in anything. One sets the idle timeout once the handshake ends: a
/// timeout belongs to the socket, not to whichever Rust value currently
/// holds it, so setting it on a clone reaches the connection no matter which
/// wrapper the returned value keeps. The other is returned alongside the
/// value, for the caller to register with `Shared` so `stop` can call
/// `shutdown` on it directly, since by the time the handshake finishes the
/// stream is boxed inside a `SecureStream` and nothing above this point can
/// reach the socket any other way. `docs/engine-contract.md` item 16c.
fn begin_handshake(
    stream: TcpStream,
    deadline: Instant,
    idle_timeout: Duration,
) -> io::Result<Negotiating> {
    let idle_handle = stream.try_clone()?;
    let registered_socket = stream.try_clone()?;
    let armed = Arc::new(AtomicBool::new(true));
    let deadline_stream = DeadlineStream {
        stream,
        deadline,
        armed: Arc::clone(&armed),
    };
    Ok(Negotiating {
        deadline_stream,
        idle_handle,
        registered_socket,
        armed,
        idle_timeout,
    })
}

impl Negotiating {
    /// Agree a version and a mode, inside the deadline that was set when
    /// this value was built.
    fn negotiate(mut self, role: Role, mode: Mode) -> Result<(Agreed, Self), TcpError> {
        let agreed = map_version(version::negotiate(&mut self.deadline_stream, role, mode))?;
        Ok((agreed, self))
    }

    /// Run one Noise step, then start the idle timeout in place of the
    /// deadline. See [`run_handshake`] for why the timeout is set on a
    /// clone rather than on the stream `run` was given.
    fn finish<T>(
        self,
        agreed: Agreed,
        run: impl FnOnce(DeadlineStream, Agreed) -> Result<T, NoiseError>,
    ) -> Result<(T, TcpStream), TcpError> {
        let Self {
            deadline_stream,
            idle_handle,
            registered_socket,
            armed,
            idle_timeout,
        } = self;
        let value = map_noise(run(deadline_stream, agreed))?;

        // The handshake is done. Turn the deadline off, and start the idle
        // timeout instead of clearing it, so a peer that later goes silent
        // does not hold this thread forever.
        armed.store(false, Ordering::SeqCst);
        idle_handle.set_read_timeout(Some(idle_timeout))?;
        idle_handle.set_write_timeout(None)?;

        Ok((value, registered_socket))
    }
}

/// Run the version exchange, then one Noise step, over `stream`, both
/// bounded by `deadline`. For a caller that already knows which pattern it
/// is about to run: every initiator does, since it is the side that picks
/// the pattern. A responder that has to read [`Agreed::mode`] first uses
/// [`begin_handshake`] and the two [`Negotiating`] steps directly instead;
/// see [`Pending::negotiate`].
fn run_handshake<T>(
    stream: TcpStream,
    role: Role,
    mode: Mode,
    deadline: Instant,
    idle_timeout: Duration,
    run: impl FnOnce(DeadlineStream, Agreed) -> Result<T, NoiseError>,
) -> Result<(T, TcpStream), TcpError> {
    let negotiating = begin_handshake(stream, deadline, idle_timeout)?;
    let (agreed, negotiating) = negotiating.negotiate(role, mode)?;
    negotiating.finish(agreed, run)
}

/// Reserves one pending-handshake slot, both overall and for one source
/// address, and gives both back when dropped.
///
/// This is the only place either counter changes. A slot can only be
/// created while one is free, and dropping it is the only way to free one
/// again, so neither count can ever be missed or double counted.
#[derive(Debug)]
struct PendingSlot {
    pending: Arc<AtomicU32>,
    pending_by_addr: Arc<Mutex<HashMap<IpAddr, u32>>>,
    addr: IpAddr,
}

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
        let mut by_addr = self
            .pending_by_addr
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = by_addr.get_mut(&self.addr) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                by_addr.remove(&self.addr);
            }
        }
    }
}

/// Accepts TCP connections and reserves a handshake slot for each one.
#[derive(Debug)]
pub struct Listener {
    inner: TcpListener,
    local_addr: SocketAddr,
    pending: Arc<AtomicU32>,
    /// How many pending handshakes are held by each source address right
    /// now. Audit `docs/audits/fable-security.md`, findings 1 and 4.
    pending_by_addr: Arc<Mutex<HashMap<IpAddr, u32>>>,
    /// [`limits::MAX_PENDING_HANDSHAKES_PER_ADDR`] in production; a test may
    /// override it with [`Listener::with_max_pending_per_addr`], the same
    /// way [`Listener::with_idle_timeout`] overrides [`IDLE_TIMEOUT_SECS`].
    max_pending_per_addr: u32,
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
            pending_by_addr: Arc::new(Mutex::new(HashMap::new())),
            max_pending_per_addr: limits::MAX_PENDING_HANDSHAKES_PER_ADDR,
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

    /// Use `max_pending_per_addr` instead of
    /// [`limits::MAX_PENDING_HANDSHAKES_PER_ADDR`].
    ///
    /// Production code never calls this. It exists so a test of the overall
    /// [`limits::MAX_PENDING_HANDSHAKES`] cap can raise the per-address one
    /// out of its way, since every client `std::net::TcpStream::connect`
    /// opens in this process shares one source address.
    #[cfg(test)]
    #[must_use]
    fn with_max_pending_per_addr(mut self, max_pending_per_addr: u32) -> Self {
        self.max_pending_per_addr = max_pending_per_addr;
        self
    }

    /// The address this listener is bound to.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Reserve a pending-handshake slot for `addr`, or refuse when
    /// [`limits::MAX_PENDING_HANDSHAKES`] are already reserved overall, or
    /// [`limits::MAX_PENDING_HANDSHAKES_PER_ADDR`] are already reserved for
    /// `addr` alone.
    ///
    /// The overall slot is given back at once when the per-address check
    /// fails, so a refused reservation never leaks one.
    fn reserve_slot(&self, addr: IpAddr) -> Result<PendingSlot, TcpError> {
        self.pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current < limits::MAX_PENDING_HANDSHAKES).then_some(current + 1)
            })
            .map_err(|_| TcpError::TooManyPending)?;
        let reserved_for_addr = {
            let mut by_addr = self
                .pending_by_addr
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let count = by_addr.entry(addr).or_insert(0);
            if *count < self.max_pending_per_addr {
                *count += 1;
                true
            } else {
                false
            }
        };
        if !reserved_for_addr {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return Err(TcpError::TooManyPendingFromAddr);
        }
        Ok(PendingSlot {
            pending: Arc::clone(&self.pending),
            pending_by_addr: Arc::clone(&self.pending_by_addr),
            addr,
        })
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
        let slot = self.reserve_slot(remote.ip())?;
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

/// A connection that has been accepted, but has not yet agreed a version, a
/// mode, or run a Noise handshake.
///
/// It holds one pending-handshake slot and the deadline its handshake must
/// finish by. [`Pending::negotiate`] runs the version exchange, inside that
/// deadline, and hands back a [`NegotiatedPending`] whose `mode()` says which
/// of its three finishers to call. The slot is freed when one of them
/// finishes, or when this value or the one it becomes is dropped without
/// calling one.
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

    /// Wait for the first byte, agree a version, and read which Noise
    /// pattern the initiator is about to run, inside the deadline.
    ///
    /// This side has no pattern of its own to request: it is the side that
    /// accepted the connection, not the side that is about to start a
    /// handshake. `Mode::Connect` is sent as the filler byte the exchange
    /// still needs; see [`version::negotiate`].
    ///
    /// Audit `docs/audits/fable-security.md`, findings 1 and 4:
    /// [`wait_for_first_byte`] runs first, so a connection that sends
    /// nothing at all is refused in
    /// [`limits::FIRST_BYTE_TIMEOUT_SECS`] seconds rather than holding its
    /// slot for the whole handshake deadline below.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Version`] when version negotiation fails, and
    /// [`TcpError::Timeout`] when the first byte or the rest of the
    /// handshake does not arrive before its deadline.
    pub fn negotiate(self) -> Result<NegotiatedPending, TcpError> {
        let Pending {
            stream,
            remote,
            slot,
            deadline,
            idle_timeout,
        } = self;
        wait_for_first_byte(&stream, deadline)?;
        let negotiating = begin_handshake(stream, deadline, idle_timeout)?;
        let (agreed, negotiating) = negotiating.negotiate(Role::Responder, Mode::Connect)?;
        Ok(NegotiatedPending {
            negotiating,
            agreed,
            remote,
            slot,
        })
    }
}

/// A [`Pending`] whose version and mode are known.
///
/// Exactly one of [`NegotiatedPending::pair`], [`NegotiatedPending::pair_ik`],
/// and [`NegotiatedPending::connect`] is the right one to call, chosen by
/// [`NegotiatedPending::mode`]. Each of the three still runs inside the same
/// deadline [`Pending::negotiate`] started with, and starts the idle timeout
/// on success.
#[derive(Debug)]
pub struct NegotiatedPending {
    negotiating: Negotiating,
    agreed: Agreed,
    remote: SocketAddr,
    slot: PendingSlot,
}

impl NegotiatedPending {
    /// The address of the peer that connected.
    #[must_use]
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }

    /// The Noise pattern the initiator is about to run.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.agreed.mode
    }

    /// The version both sides agreed on.
    #[must_use]
    pub fn version(&self) -> u16 {
        self.agreed.version
    }

    /// Run Noise XX as responder, inside the deadline [`Pending::negotiate`]
    /// started with. Starts the idle timeout on success.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Noise`] when the handshake fails, and
    /// [`TcpError::Timeout`] when it does not finish before the deadline.
    pub fn pair(self, key: &StaticKey) -> Result<PairedConnection, TcpError> {
        debug_assert_eq!(
            self.agreed.mode,
            Mode::PairByCode,
            "the caller should have matched on mode() before calling pair()"
        );
        let Self {
            negotiating,
            agreed,
            slot: _slot,
            ..
        } = self;
        // Pairing runs once, while a person is watching both screens, and
        // already has its own timeout; it does not register for `stop` to
        // close directly the way an ordinary connection does.
        let (connection, _socket) = negotiating.finish(agreed, |s, agreed| {
            noise::pair_as_responder(s, key, &agreed.prologue).map(|paired| PairedConnection {
                paired,
                version: agreed.version,
            })
        })?;
        Ok(connection)
    }

    /// Run Noise IK as responder, inside the deadline [`Pending::negotiate`]
    /// started with. Starts the idle timeout on success.
    ///
    /// `expected_nonce` is the nonce of the offer this side is currently
    /// showing, or `None` when it is not offering to pair by QR at all. See
    /// [`noise::pair_ik_as_responder`].
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Noise`] when the handshake fails, including when
    /// the initiator's nonce does not match `expected_nonce`, and
    /// [`TcpError::Timeout`] when it does not finish before the deadline.
    pub fn pair_ik(
        self,
        key: &StaticKey,
        expected_nonce: Option<&[u8; noise::QR_NONCE_LEN]>,
    ) -> Result<IkPairedConnection, TcpError> {
        debug_assert_eq!(
            self.agreed.mode,
            Mode::PairByQr,
            "the caller should have matched on mode() before calling pair_ik()"
        );
        let Self {
            negotiating,
            agreed,
            slot: _slot,
            ..
        } = self;
        // As `pair`: a QR pairing handshake runs once, while the Mac is
        // showing `Requested`, and has its own deadline.
        let (connection, _socket) = negotiating.finish(agreed, |s, agreed| {
            noise::pair_ik_as_responder(s, key, expected_nonce, &agreed.prologue).map(|accepted| {
                IkPairedConnection {
                    accepted,
                    version: agreed.version,
                }
            })
        })?;
        Ok(connection)
    }

    /// Run Noise KK as responder, trying each of `candidates` in turn
    /// against the one message the peer sends, and binding the first that
    /// authenticates, inside the deadline [`Pending::negotiate`] started
    /// with. Starts the idle timeout on success.
    ///
    /// The wire says nothing about who is calling before the handshake, and
    /// message one can only be read once, so every candidate the caller
    /// might be is tried against that same read. `docs/engine-contract.md`
    /// item 16b.
    ///
    /// # Errors
    ///
    /// Returns [`TcpError::Noise`] when no candidate authenticates, and
    /// [`TcpError::Timeout`] when it does not finish before the deadline.
    pub fn connect(
        self,
        key: &StaticKey,
        candidates: &[PublicKey],
    ) -> Result<Connection, TcpError> {
        debug_assert_eq!(
            self.agreed.mode,
            Mode::Connect,
            "the caller should have matched on mode() before calling connect()"
        );
        let Self {
            negotiating,
            agreed,
            remote,
            slot: _slot,
        } = self;
        let ((stream, peer, version), socket) = negotiating.finish(agreed, |s, agreed| {
            noise::connect_as_responder_any(s, key, candidates, &agreed.prologue)
                .map(|(stream, peer)| (stream, peer, agreed.version))
        })?;
        Ok(Connection {
            stream,
            remote,
            version,
            peer,
            socket,
        })
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
    /// A clone of the raw socket, taken before `stream` boxed it. Register
    /// this with `Shared` so `stop` can call `shutdown` on it directly; that
    /// is the only way to reach the socket once this value exists.
    /// `docs/engine-contract.md` item 16c.
    pub socket: TcpStream,
}

/// What a successful [`pair`] or [`NegotiatedPending::pair`] produces.
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

/// What a successful [`pair_ik`] or [`NegotiatedPending::pair_ik`] produces.
///
/// [`IkAccepted`] already carries the stream, the peer, the nonce, and the
/// hello; this only adds the version, for symmetry with [`PairedConnection`]
/// and [`Connection`].
#[derive(Debug)]
pub struct IkPairedConnection {
    /// The encrypted channel, the peer, the nonce, and the hello, exactly as
    /// Noise IK produced them.
    pub accepted: IkAccepted,
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
    // F3: a plain `TcpStream::connect` to an address nothing answers can
    // cost tens of seconds of the OS's own connect timeout before it gives
    // up, well past what a caller waiting on this handshake should ever
    // sit through. `pair_ik` already bounds its own connect step this way.
    let stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS))
            .map_err(|error| {
                if is_timeout(&error) {
                    TcpError::Timeout
                } else {
                    TcpError::Io(error)
                }
            })?;
    // As in `Listener::accept`: without this, a small write waits on a
    // delayed acknowledgement instead of reaching the wire at once.
    let _ = stream.set_nodelay(true);
    let remote = stream.peer_addr()?;
    let deadline = Instant::now() + Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS);
    let ((stream, version), socket) = run_handshake(
        stream,
        Role::Initiator,
        Mode::Connect,
        deadline,
        Duration::from_secs(IDLE_TIMEOUT_SECS),
        |s, agreed| {
            noise::connect_as_initiator(s, key, peer, &agreed.prologue)
                .map(|stream| (stream, agreed.version))
        },
    )?;
    Ok(Connection {
        stream,
        remote,
        version,
        peer: *peer,
        socket,
    })
}

/// Dial a peer, agree a version, and run XX as initiator.
///
/// # Errors
///
/// As [`connect`], except there is no stored peer key to check, so a
/// [`TcpError::Noise`] here means the handshake itself failed rather than
/// that the peer's identity was wrong.
pub fn pair(addr: SocketAddr, key: &StaticKey) -> Result<PairedConnection, TcpError> {
    // F3: as in `connect`, bound the connect step itself rather than let it
    // run for however long the OS's own default timeout takes.
    let stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS))
            .map_err(|error| {
                if is_timeout(&error) {
                    TcpError::Timeout
                } else {
                    TcpError::Io(error)
                }
            })?;
    // As in `Listener::accept`: without this, a small write waits on a
    // delayed acknowledgement instead of reaching the wire at once.
    let _ = stream.set_nodelay(true);
    let deadline = Instant::now() + Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS);
    // Pairing does not register a socket for `stop` to close; see
    // `NegotiatedPending::pair`.
    let (connection, _socket) = run_handshake(
        stream,
        Role::Initiator,
        Mode::PairByCode,
        deadline,
        Duration::from_secs(IDLE_TIMEOUT_SECS),
        |s, agreed| {
            noise::pair_as_initiator(s, key, &agreed.prologue).map(|paired| PairedConnection {
                paired,
                version: agreed.version,
            })
        },
    )?;
    Ok(connection)
}

/// Dial the address from a scanned QR offer, agree a version, and run IK as
/// initiator, with the offer's nonce and this device's hello in message one.
///
/// Unlike [`pair`] and [`connect`], the connect step itself is bounded, by
/// [`limits::QR_ADDRESS_CONNECT_TIMEOUT_SECS`]: an offer can list more than
/// one address, tried in order by the caller, and a plain, unbounded
/// connect to an address nothing answers can otherwise cost tens of
/// seconds of the OS's own connect timeout before it gives up, which adds
/// up fast across a handful of wrong addresses ahead of the right one.
///
/// # Errors
///
/// As [`pair`], plus [`TcpError::Timeout`] when the connect step itself
/// does not finish within [`limits::QR_ADDRESS_CONNECT_TIMEOUT_SECS`].
/// [`TcpError::Noise`] means the responder did not hold the private key
/// matching the static key from the offer, which is what stops an attacker
/// without it from completing this handshake at all.
#[allow(clippy::too_many_arguments)]
pub fn pair_ik(
    addr: SocketAddr,
    key: &StaticKey,
    responder: &PublicKey,
    nonce: &[u8; noise::QR_NONCE_LEN],
    my_name: &str,
    my_kind: DeviceKind,
) -> Result<SecureStream, TcpError> {
    let stream = TcpStream::connect_timeout(
        &addr,
        Duration::from_secs(limits::QR_ADDRESS_CONNECT_TIMEOUT_SECS),
    )
    .map_err(|error| {
        if is_timeout(&error) {
            TcpError::Timeout
        } else {
            TcpError::Io(error)
        }
    })?;
    // As in `Listener::accept`: without this, a small write waits on a
    // delayed acknowledgement instead of reaching the wire at once.
    let _ = stream.set_nodelay(true);
    let deadline = Instant::now() + Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS);
    // As `pair`: a QR pairing handshake does not register a socket for
    // `stop` to close.
    let (stream, _socket) = run_handshake(
        stream,
        Role::Initiator,
        Mode::PairByQr,
        deadline,
        Duration::from_secs(IDLE_TIMEOUT_SECS),
        |s, agreed| {
            noise::pair_ik_as_initiator(
                s,
                key,
                responder,
                nonce,
                my_name,
                my_kind,
                &agreed.prologue,
            )
        },
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::{Listener, Pending, TcpError, connect, pair, pair_ik};
    use crate::limits;
    use crate::noise::{QR_NONCE_LEN, StaticKey};
    use crate::peers::DeviceKind;
    use crate::version::{MAGIC, Mode, VERSION_MAX};
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
            pending.negotiate().unwrap().pair(&key_responder).unwrap()
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
            let mut connection = pending
                .negotiate()
                .unwrap()
                .connect(&key_b, &[public_a])
                .unwrap();
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
    #[ignore = "sends packets to a private address off this machine and waits the full timeout; run on demand"]
    fn connect_to_a_non_routable_address_times_out_within_the_handshake_timeout() {
        // F3: `connect` used to reach for a plain `TcpStream::connect`,
        // whose own timeout is however long the OS takes to decide nothing
        // is there, tens of seconds on some networks. It must give up
        // within `HANDSHAKE_TIMEOUT_SECS` instead, as `pair_ik` already
        // does for its own connect step.
        let unroutable = SocketAddr::from(([10, 255, 255, 1], 9));
        let key = StaticKey::generate().unwrap();
        let peer_public = StaticKey::generate().unwrap().public();

        let started = Instant::now();
        let result = connect(unroutable, &key, &peer_public);
        let took = started.elapsed();

        assert!(result.is_err(), "nothing answers this address");
        assert!(
            took < Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS) + Duration::from_secs(1),
            "the connect step must give up within the handshake timeout, took {took:?}"
        );
    }

    #[test]
    #[ignore = "sends packets to a private address off this machine and waits the full timeout; run on demand"]
    fn pair_to_a_non_routable_address_times_out_within_the_handshake_timeout() {
        // F3: as the test above, for `pair`'s own connect step.
        let unroutable = SocketAddr::from(([10, 255, 255, 1], 9));
        let key = StaticKey::generate().unwrap();

        let started = Instant::now();
        let result = pair(unroutable, &key);
        let took = started.elapsed();

        assert!(result.is_err(), "nothing answers this address");
        assert!(
            took < Duration::from_secs(limits::HANDSHAKE_TIMEOUT_SECS) + Duration::from_secs(1),
            "the connect step must give up within the handshake timeout, took {took:?}"
        );
    }

    #[test]
    fn the_ninth_pending_handshake_is_refused() {
        // Every client below connects from this one process, so they all
        // share one source address. The per-address cap this audit's fix
        // adds would refuse the third of them long before the ninth, so it
        // is raised out of the way here. `ferry-runtime`'s
        // `tests/security_bounds.rs` is what tests the per-address cap
        // itself, over a real accept loop.
        let listener = Listener::bind(local_any())
            .unwrap()
            .with_max_pending_per_addr(limits::MAX_PENDING_HANDSHAKES);
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
        // now shows up once the version exchange actually runs, inside
        // `negotiate`. A client that sends nothing never gets far enough for
        // `pair` or `connect` to matter.
        let pending = listener.accept().unwrap();
        match pending.negotiate() {
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
            pending
                .negotiate()?
                .connect(&key_server, &[public_client])
                .map(|_| ())
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

            // The 8 byte version and mode exchange arrives all at once, so
            // `negotiate` finishes quickly and the deadline still has time
            // left over for the Noise handshake that follows.
            let mut version_bytes = Vec::with_capacity(8);
            version_bytes.extend_from_slice(&MAGIC);
            version_bytes.extend_from_slice(&VERSION_MAX.to_be_bytes());
            version_bytes.push(0); // Mode::Connect.
            if stream.write_all(&version_bytes).is_err() {
                return;
            }

            // A valid-looking handshake length prefix, then a body dripped
            // one byte every 200ms. A socket timeout that resets on every
            // read would let this alone take well over a second; the body
            // never fully arrives either way.
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
        let result = pending
            .negotiate()
            .and_then(|negotiated| negotiated.connect(&key, &[key.public()]));
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
        let silent = thread::spawn(move || silent_pending.negotiate().map(|_| ()));
        let key_responder = StaticKey::generate().unwrap();
        let responder = thread::spawn(move || {
            second_pending
                .negotiate()
                .unwrap()
                .pair(&key_responder)
                .unwrap()
        });

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
            let mut connection = pending.negotiate().unwrap().pair(&key_responder).unwrap();
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

    #[test]
    fn negotiate_reports_the_mode_the_dialer_used() {
        // Three dialers, three modes, over the same listener. The wire says
        // which pattern is coming before a single Noise byte is read.
        let listener = Listener::bind(local_any()).unwrap();
        let addr = listener.local_addr();

        let connect_client = thread::spawn(move || {
            let stranger = StaticKey::generate().unwrap();
            let _ = connect(addr, &stranger, &stranger.public());
        });
        let negotiated = listener.accept().unwrap().negotiate().unwrap();
        assert_eq!(negotiated.mode(), Mode::Connect);
        assert_eq!(negotiated.version(), VERSION_MAX);
        drop(negotiated);
        connect_client.join().unwrap();

        let pair_client = thread::spawn(move || {
            let key = StaticKey::generate().unwrap();
            let _ = pair(addr, &key);
        });
        let negotiated = listener.accept().unwrap().negotiate().unwrap();
        assert_eq!(negotiated.mode(), Mode::PairByCode);
        drop(negotiated);
        pair_client.join().unwrap();

        let responder_key = StaticKey::generate().unwrap();
        let responder_public = responder_key.public();
        let pair_ik_client = thread::spawn(move || {
            let key = StaticKey::generate().unwrap();
            let nonce = [7u8; QR_NONCE_LEN];
            let _ = pair_ik(
                addr,
                &key,
                &responder_public,
                &nonce,
                "Pixel 3 XL",
                DeviceKind::Phone,
            );
        });
        let negotiated = listener.accept().unwrap().negotiate().unwrap();
        assert_eq!(negotiated.mode(), Mode::PairByQr);
        drop(negotiated);
        pair_ik_client.join().unwrap();
    }

    #[test]
    fn a_qr_pairing_over_tcp_completes_and_reports_the_hello() {
        let listener = Listener::bind(local_any()).unwrap();
        let addr = listener.local_addr();

        let key_responder = StaticKey::generate().unwrap();
        let public_responder = key_responder.public();
        let nonce = [3u8; QR_NONCE_LEN];
        let server = thread::spawn(move || {
            let pending = listener.accept().unwrap();
            let negotiated = pending.negotiate().unwrap();
            assert_eq!(negotiated.mode(), Mode::PairByQr);
            negotiated.pair_ik(&key_responder, Some(&nonce)).unwrap()
        });

        let key_initiator = StaticKey::generate().unwrap();
        let mut initiator_stream = pair_ik(
            addr,
            &key_initiator,
            &public_responder,
            &nonce,
            "Pixel 3 XL",
            DeviceKind::Phone,
        )
        .unwrap();

        let mut connection = server.join().unwrap();
        assert_eq!(connection.accepted.peer, key_initiator.public());
        assert_eq!(connection.accepted.name, "Pixel 3 XL");
        assert_eq!(connection.accepted.kind, DeviceKind::Phone);

        initiator_stream.write_all(b"scanned").unwrap();
        let mut buf = [0u8; 7];
        connection.accepted.stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"scanned");
    }
}
