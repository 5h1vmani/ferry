//! Finding the phone on the local network with mDNS.
//!
//! Not implemented yet. The contract below is the specification.
//!
//! # Contract
//!
//! Rules from `docs/protocol.md` section 10. Only the phone advertises. The
//! instance name is random. The TXT record carries the protocol version and
//! nothing else, never a key or a fingerprint. Advertising can be stopped.
//!
//! Built on the `mdns-sd` crate, which needs no async runtime.
//!
//! Public shape:
//!
//! ```text
//! pub const SERVICE_TYPE: &str = "_ferry._tcp.local.";
//!
//! /// The phone side. Announces one service until dropped or stopped.
//! pub struct Advertiser { .. }
//! impl Advertiser {
//!     /// Announce on `port` under a fresh random instance name.
//!     pub fn start(port: u16) -> Result<Self, DiscoveryError>;
//!     pub fn instance_name(&self) -> &str;
//!     pub fn stop(self);
//! }
//!
//! /// The Mac side. Reports services as they appear and disappear.
//! pub struct Browser { .. }
//! impl Browser {
//!     pub fn start() -> Result<Self, DiscoveryError>;
//!     /// Block up to `timeout` for the next event.
//!     pub fn next(&self, timeout: Duration) -> Option<Event>;
//!     pub fn stop(self);
//! }
//!
//! pub enum Event {
//!     Found { instance: String, addr: SocketAddr, version: u16 },
//!     Lost { instance: String },
//! }
//! ```
//!
//! A test advertises and browses inside one process. It is marked
//! `#[ignore]` with a reason, because it needs a real multicast-capable
//! interface and CI runners do not reliably have one. It is run by hand.
