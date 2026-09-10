//! The network transport: TCP, with the handshake limits enforced.
//!
//! Not implemented yet. The contract below is the specification.
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
