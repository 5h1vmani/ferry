//! The engine both apps link.
//!
//! `ferry-core` is a library of protocol pieces. This crate composes them into
//! one object, [`Engine`], with one callback, [`EngineListener`], and exposes
//! both through `UniFFI` so the Swift and Kotlin apps stay thin. Every decision
//! lives on this side of the boundary. The apps show state and forward taps.
//!
//! # Shape of the boundary
//!
//! The app calls methods. The engine reports change through the listener.
//! A change notification carries little or nothing; the app then asks again
//! with [`Engine::devices`] or [`Engine::transfers`]. That keeps big values
//! from crossing the boundary on every tick and keeps the callback surface
//! small.
//!
//! Errors cross as [`FerryError`], whose `code` is the Rust path of the
//! underlying variant, such as `OpError::NotFound`. The apps map a code to
//! words through the table generated from `design/errors.json`. No English is
//! composed on this side.
//!
//! Keys cross as bytes. The platform owns their storage: the Keychain on macOS
//! and private storage on Android. The engine wipes what it is given when it
//! is dropped.
//!
//! # Threads
//!
//! Blocking I/O with threads, per decision record 7. `start` spawns: one
//! accept loop that hands each connection to its own thread; one discovery
//! browser; and a pool of four transfer workers. `stop` joins them. A
//! transfer beyond the fourth waits in a queue and is reported as `Queued`.
//!
//! # The app must call `stop`
//!
//! [`Engine::stop`] is what ends the engine, not dropping it. A listener
//! that holds the engine, which is the ordinary shape in both apps, is a
//! ring of references, so the engine is never dropped and `Drop` never runs.
//!
//! After `stop` returns, no method of [`EngineListener`] is called again,
//! no file is served to any device, and the data directory is free for
//! another engine. One narrow case remains: a serving thread that ends in
//! the same instant may make one last `devices_changed` call, because such
//! a thread is not joined. See the known limitation below. One engine at a time may use a data directory;
//! [`Engine::new`] refuses the second with `Runtime::BadConfig` and names
//! the file to clear if a crash left one behind.
//!
//! Notifications are rationed. [`EngineListener::devices_changed`] and
//! [`EngineListener::transfers_changed`] arrive at most once every 250
//! milliseconds each, however much moved, and the last change always
//! arrives. Pairing states are not rationed, because each one carries a
//! value the screen needs.
//!
//! # Roles
//!
//! One engine type serves both devices. The phone calls
//! [`Engine::set_reachable`] to advertise and accept. The Mac calls
//! [`Engine::start_pairing`] to browse and, once paired, [`Engine::list`] to
//! browse the phone's shared folder and [`Engine::pull`] to fetch a file.
//! Nothing in the type knows which device it is on.
//!
//! # Not yet
//!
//! Push, from the Mac to the phone, is not built. The core has `pull` only.
//! Per-peer connection caps are not built. Both are recorded in `PLAN.md`.
//!
//! # Contract for `Engine`
//!
//! ```text
//! #[derive(uniffi::Object)]
//! pub struct Engine { .. }
//!
//! #[uniffi::export]
//! impl Engine {
//!     /// Build an engine. Does not start any thread.
//!     #[uniffi::constructor]
//!     pub fn new(config: Config, listener: Box<dyn EngineListener>) -> Result<Arc<Self>, FerryError>;
//!
//!     /// Make a fresh key pair for first run. The app stores it.
//!     #[uniffi::constructor]  (or a free exported fn)
//!     pub fn generate_key() -> KeyPair;
//!
//!     /// Bind the listener, start the accept loop, start the adb poll.
//!     pub fn start(&self) -> Result<(), FerryError>;
//!     /// Stop everything and join every thread. Safe to call twice.
//!     pub fn stop(&self);
//!
//!     /// Phone only in practice. On: advertise over mDNS and accept KK
//!     /// connections from paired devices. Off: stop advertising; existing
//!     /// connections finish, new ones are refused.
//!     pub fn set_reachable(&self, on: bool);
//!
//!     pub fn devices(&self) -> Vec<DeviceInfo>;
//!     /// Forget a device: remove its key and every transfer record for it.
//!     pub fn forget(&self, key_hex: String) -> Result<(), FerryError>;
//!
//!     /// Enter pairing. The Mac browses mDNS and polls adb, and reports
//!     /// candidates through the listener. The phone accepts one XX handshake
//!     /// and reports the code. Times out after two minutes.
//!     pub fn start_pairing(&self);
//!     /// Mac: dial the chosen candidate and run XX. The code is reported
//!     /// through the listener.
//!     pub fn pick_candidate(&self, id: String) -> Result<(), FerryError>;
//!     /// Both sides. Accept stores the peer and sends hello. Reject drops it.
//!     pub fn confirm_pairing(&self, accept: bool);
//!     pub fn cancel_pairing(&self);
//!
//!     pub fn transfers(&self) -> Vec<TransferInfo>;
//!     /// Fetch one file from a paired device into the shared root, under
//!     /// `local_name`. Returns the transfer id. Runs on its own thread and
//!     /// reports through the listener. Resumes on its own when the device
//!     /// becomes reachable again.
//!     pub fn pull(&self, device_key_hex: String, remote_path: String, local_name: String) -> Result<String, FerryError>;
//!     /// Retry a failed transfer from its resume point.
//!     pub fn retry(&self, transfer_id: String) -> Result<(), FerryError>;
//!     /// List one folder on a paired device. Pages through the server's
//!     /// cursor on its own and returns every entry. Blocks for the round
//!     /// trip, so the app calls it off the main thread.
//!     pub fn list(&self, device_key_hex: String, remote_path: String) -> Result<Vec<Entry>, FerryError>;
//! }
//! ```
//!
//! The pairing state machine: Idle, Waiting, Found (Mac, with candidates),
//! Code (both, with the six digits), Confirmed, Failed. Exactly one pairing at
//! a time. The listener receives every state change.
//!
//! Transports: the engine tries USB first when adb shows an authorised device
//! that has a Ferry forward, then Wi-Fi from the last known address, then
//! mDNS. Whichever connects becomes `reachable_via`. Speed is bytes moved in
//! the last second, updated no more than once a second.
//!
//! # Choices this build made
//!
//! Discovery runs for the whole session, not only while pairing. A paired
//! phone changes address every time it rejoins a network, and the Mac has to
//! find it again with nobody doing anything. See job 1 and job 3 in
//! `docs/jobs.md`. Candidates are filtered: an address is only offered to the
//! pairing screen while pairing is open.
//!
//! After pairing, the side that accepted the connection keeps serving on it.
//! The side that dialed lets the stream go and dials again when it needs to.
//! Two servers on one stream would each wait for the other to speak.
//!
//! # Known limitation: picking the peer for an inbound connection
//!
//! Every connection after pairing runs Noise KK, and KK needs the caller's
//! static public key before the handshake starts. The wire carries nothing
//! that says who is calling, and a handshake cannot be tried twice on one
//! stream: the version exchange and the first Noise message are already read
//! by then, and `Pending::connect` consumes the connection.
//!
//! So this build guesses, and never weakens the handshake to avoid guessing.
//! With one stored peer it uses that peer. With more it uses the peer whose
//! last known address matches the caller's address, and otherwise the first
//! peer in key order. A wrong guess fails the handshake and the connection is
//! dropped, which is safe but costs the caller a retry.
//!
//! The real fix is a responder that reads the first KK message, then tries
//! each stored key against it. That needs a change in `tcp.rs` and `noise.rs`,
//! so it is phase 2 work.
//!
//! # Known limitation: a serving thread cannot be woken
//!
//! `stop` joins the accept loop, the discovery loop, the `adb` poll, the
//! pairing watchdog, and every transfer worker. It does not join threads that
//! are serving a connection. Such a thread is blocked reading from an
//! encrypted stream, and the socket handle sits inside that stream where this
//! crate cannot reach it. So it ends when the peer goes away or when the idle
//! timeout in `tcp.rs` fires, whichever comes first.
//!
//! `stop` and `forget` have the same shape of problem and solve it the same
//! way. Neither can close the socket, so both switch the served filesystem
//! off instead. Every operation on that connection is refused from that
//! moment, and `stop` also takes the shared root away, so a socket that
//! stays open until its idle timeout serves nothing at all. The switch is
//! put in place as soon as the handshake proves who is calling, before names
//! are exchanged, so a peer that delays its `hello` is still within reach of
//! `forget`.
//!
//! A transfer thread is joined, and it ends quickly, because the stream it
//! reads through fails as soon as the stop flag is set. The one case that
//! still waits is a transfer caught inside a handshake, which `tcp.rs` bounds
//! at ten seconds.

uniffi::setup_scaffolding!();

mod engine;
pub mod errors;
mod guard;
mod notify;
mod record;
mod state;
mod transfer;

pub use engine::{Engine, FERRY_PHONE_PORT, generate_key};

use std::fmt;

/// What the app tells the engine at construction.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Config {
    /// Where the engine keeps its own files: paired devices and transfer
    /// records. Must exist and be private to the app.
    pub data_dir: String,
    /// The folder served to paired devices, and where pulled files land.
    pub shared_root: String,
    /// The name sent in `hello`. At most 64 bytes. Defaults to the model.
    pub display_name: String,
    /// The port to listen on. Zero means any free port.
    pub listen_port: u16,
    /// This device's long-lived key. The app loaded it from secure storage.
    pub key: KeyPair,
}

/// A static key pair as bytes. The platform stores it; the engine uses it.
#[derive(Clone, uniffi::Record)]
pub struct KeyPair {
    /// Thirty-two bytes. Never logged, never shown.
    pub private: Vec<u8>,
    /// Thirty-two bytes. Safe to show as a fingerprint.
    pub public: Vec<u8>,
}

impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The private half must never reach a log.
        f.debug_struct("KeyPair")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

/// One way a device is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Transport {
    /// Through the adb tunnel over a cable.
    Usb,
    /// Over the local network.
    Wifi,
}

/// One paired device, as the Devices screen shows it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DeviceInfo {
    /// The public key as 64 lowercase hex characters. This is the identity.
    pub key_hex: String,
    /// The name it sent in `hello`. Shown, never trusted.
    pub name: String,
    /// When it was paired.
    pub paired_unix_secs: i64,
    /// How it is reachable right now, if at all.
    pub reachable_via: Option<Transport>,
    /// Bytes per second moving to or from it in the last second, if any.
    pub speed_bytes_per_sec: Option<u64>,
    /// When it was last reachable, if it is not reachable now.
    pub last_seen_unix_secs: Option<i64>,
}

/// Where a transfer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TransferState {
    /// Waiting for a thread or for the device to be reachable.
    Queued,
    /// Moving bytes.
    Active,
    /// The link dropped. Resumes on its own when the device is reachable.
    Paused,
    /// Every chunk verified and the file is at its final name.
    Done,
    /// Stopped for a reason in `error`. A retry starts from the resume point.
    Failed,
}

/// One transfer, as the Transfers screen shows it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TransferInfo {
    /// Stable for the life of the transfer, across restarts.
    pub id: String,
    /// Which device it is with.
    pub device_key_hex: String,
    /// The file's name, for display.
    pub file_name: String,
    /// Total size in bytes.
    pub bytes_total: u64,
    /// Bytes verified in place.
    pub bytes_done: u64,
    /// Where it is.
    pub state: TransferState,
    /// Which transport is carrying it, while active.
    pub transport: Option<Transport>,
    /// Why it failed or paused, when it did.
    pub error: Option<FerryError>,
}

/// What kind of thing an [`Entry`] names, mirroring
/// `ferry_core::ops::FileKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

/// One file or directory inside another device's shared root, as
/// [`Engine::list`] returns it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Entry {
    /// The entry's name within its parent directory. Never a full path.
    pub name: String,
    /// Whether the entry is a file or a directory.
    pub kind: EntryKind,
    /// The size in bytes.
    pub size: u64,
    /// The last modification time, in seconds since the Unix epoch.
    pub modified_unix_secs: i64,
}

/// A device that could be paired, as the Mac's pairing screen lists it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PairingCandidate {
    /// Opaque, for `pick_candidate`.
    pub id: String,
    /// How it was found.
    pub transport: Transport,
    /// The last four characters of its random mDNS name, or the adb serial's
    /// last four. The phone shows the same four so a person can match them.
    pub short_code: String,
}

/// Where pairing is.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum PairingState {
    /// Not pairing.
    Idle,
    /// Looking for a device, or on the phone, waiting for a Mac.
    Waiting,
    /// The Mac has candidates to pick from.
    Found {
        /// What can be picked.
        candidates: Vec<PairingCandidate>,
    },
    /// Both screens show the code.
    Code {
        /// Six digits, zero padded.
        code: String,
    },
    /// Both sides confirmed. The device is now in `devices()`.
    Confirmed {
        /// The new device.
        device: DeviceInfo,
    },
    /// Pairing stopped.
    Failed {
        /// Why.
        error: FerryError,
    },
}

/// An error crossing to the app.
///
/// `code` is the Rust path of the underlying variant, such as
/// `OpError::NotFound` or `TcpError::Timeout`. The app maps it to words
/// through the table generated from `design/errors.json`. `detail` carries a
/// value the words may need, such as a file name, and is optional.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FerryError {
    /// The only variant. A code and an optional detail.
    #[error("{code}")]
    Failed {
        /// The Rust path of the variant, used as the lookup key.
        code: String,
        /// A value the message may need. Never a sentence.
        detail: Option<String>,
    },
}

/// How the engine tells the app something changed.
///
/// Every method is called from an engine thread, never from the thread the
/// app called into. The app must hop to its main thread before touching a
/// view.
#[uniffi::export(callback_interface)]
pub trait EngineListener: Send + Sync {
    /// The device list, or a device's reachability or speed, changed.
    fn devices_changed(&self);
    /// A transfer was added, moved, or changed state.
    fn transfers_changed(&self);
    /// Pairing moved to a new state.
    fn pairing_changed(&self, state: PairingState);
}
