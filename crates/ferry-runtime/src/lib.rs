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
//! [`Engine::start_pairing_with`] to browse and, once paired, [`Engine::list`] to
//! browse the phone's shared folder and [`Engine::pull`] to fetch a file.
//! Nothing in the type knows which device it is on.
//!
//! # Not yet
//!
//! Per-peer connection caps are not built. Recorded in `PLAN.md`.
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
//!     /// Whether this engine accepts connections, what port it listens on,
//!     /// and whether `adb` was found.
//!     pub fn status(&self) -> Status;
//!
//!     /// The app reports the name of the Wi-Fi network it is on, or None
//!     /// when it cannot read one. Called after start and on every change.
//!     pub fn set_network(&self, name: Option<String>);
//!     /// Adds a name to the trusted list. An empty name, a name over 32
//!     /// bytes, or a 33rd name is refused with `Runtime::NetworkName`.
//!     pub fn trust_network(&self, name: String) -> Result<(), FerryError>;
//!     pub fn forget_network(&self, name: String) -> Result<(), FerryError>;
//!     pub fn trusted_networks(&self) -> Vec<String>;
//!
//!     pub fn devices(&self) -> Vec<DeviceInfo>;
//!     /// Forget a device: remove its key and every transfer record for it.
//!     pub fn forget(&self, key_hex: String) -> Result<(), FerryError>;
//!
//!     /// The roots currently served.
//!     pub fn roots(&self) -> Vec<Root>;
//!     /// Replace the served roots. Reaches every already-connected peer on
//!     /// its next operation; nobody needs to reconnect.
//!     pub fn set_roots(&self, roots: Vec<Root>) -> Result<(), FerryError>;
//!     /// Change where a pulled file lands, making the folder if needed.
//!     pub fn set_download_dir(&self, path: String) -> Result<(), FerryError>;
//!
//!     /// Enter pairing, by the given method. Replaces `start_pairing`.
//!     /// `Code`: the Mac browses mDNS and polls adb, and reports candidates
//!     /// through the listener; the phone accepts one XX handshake and
//!     /// reports the code. `Qr`: the Mac makes a nonce and an offer, and
//!     /// accepts one IK handshake whose nonce matches it. Both time out
//!     /// after two minutes.
//!     pub fn start_pairing_with(&self, method: PairingMethod);
//!     /// Mac: dial the chosen candidate and run XX. The code is reported
//!     /// through the listener.
//!     pub fn pick_candidate(&self, id: String) -> Result<(), FerryError>;
//!     /// Phone only. The bytes its camera decoded from the Mac's QR code.
//!     /// Dials the offer's addresses and runs IK. Ends in `Confirmed` or
//!     /// `Failed`; the phone never shows `Requested`.
//!     pub fn offer_scanned(&self, payload: Vec<u8>) -> Result<(), FerryError>;
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
//!     /// Copy a whole folder. Lists it over the connection, then queues one
//!     /// transfer per file under a new batch. Blocks until the listing is
//!     /// done, so the app calls it off the main thread, as it does `list`.
//!     pub fn pull_folder(&self, device_key_hex: String, remote_path: String) -> Result<String, FerryError>;
//!     /// Send one file to a paired device. Returns the transfer id. Builds
//!     /// the file's manifest first, resumes like a pull, and writes through
//!     /// `<remote_path>.ferry-part` until the whole file verifies.
//!     pub fn push(&self, device_key_hex: String, local_path: String, remote_path: String) -> Result<String, FerryError>;
//!     /// Send several files into one folder on a paired device, as one
//!     /// batch labelled with that folder. Returns the batch id.
//!     pub fn push_files(&self, device_key_hex: String, local_paths: Vec<String>, remote_folder: String) -> Result<String, FerryError>;
//!     pub fn batches(&self) -> Vec<BatchInfo>;
//!     /// Retry every `Failed` transfer in a batch.
//!     pub fn retry_batch(&self, batch_id: String) -> Result<(), FerryError>;
//!
//!     /// The access log, newest first. `None` for `device_key_hex` returns
//!     /// every device's. `limit` is capped at 1,000.
//!     pub fn access_log(&self, device_key_hex: Option<String>, limit: u32) -> Vec<AccessEntry>;
//!
//!     /// Starts serving a device's roots over WebDAV. Idempotent.
//!     pub fn mount_start(&self, device_key_hex: String) -> Result<MountEndpoint, FerryError>;
//!     /// Stops serving a device's roots over WebDAV.
//!     pub fn mount_stop(&self, device_key_hex: String);
//!     /// The app reports where the OS mounted a device's bridge, or `None`
//!     /// once it unmounted it.
//!     pub fn set_mount_path(&self, device_key_hex: String, path: Option<String>) -> Result<(), FerryError>;
//! }
//! ```
//!
//! The pairing state machine: Idle, Waiting, Found (Mac, with candidates),
//! Code (both, with the six digits), Offering (Mac, with the QR payload),
//! Requested (Mac, with the scanning phone's name), Confirmed, Failed. Waiting,
//! Found, and Code belong to the code method; Offering and Requested belong to
//! the QR method. Exactly one pairing at a time. The listener receives every
//! state change.
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
//! # Picking the peer for an inbound connection
//!
//! Every connection after pairing runs Noise KK, and KK needs the caller's
//! static public key before the handshake starts. The wire carries nothing
//! that says who is calling, and a handshake message can only be read once:
//! the stream is not rewound to try a second guess.
//!
//! So the responder never guesses. It reads the first KK message once, with
//! `noise.rs`'s `read_kk_message_one`, then tries every stored peer against
//! that same read in turn, the one whose last known address matches the
//! caller's address first, then the rest, and binds the first that
//! authenticates. `docs/engine-contract.md` item 16b; see `candidate_peers`
//! in `engine.rs` and `Pending::connect` in `tcp.rs`.
//!
//! # Closing a connection from `stop`
//!
//! A thread can be blocked in a kernel read, deep inside an encrypted
//! stream, where the stopping flag `stop` sets is invisible to it: a read
//! only checks the flag before it starts, not while it waits. Left
//! unfixed, such a thread ends only when the peer goes away or the idle
//! timeout in `tcp.rs` fires, whichever comes first, so `stop` could wait
//! up to five minutes for one silent peer.
//!
//! Every connection now registers a clone of its raw socket in `Shared`,
//! keyed by a connection id from the same counter the access log uses, as
//! soon as it is established, both dialled and accepted, and removes it
//! when the connection ends. `stop` sets the flag, then calls
//! `shutdown(Both)` on every socket still registered, before it joins the
//! threads it always has. A blocked read then fails at once, so the thread
//! sees the flag on its very next check. `docs/engine-contract.md` item
//! 16c; see `Shared::register_socket` and `Engine::stop` in `engine.rs`.
//!
//! A serving thread is still not joined: `accept_loop` spawns it and lets
//! it go, since it may otherwise outlive an idle timeout `stop` should not
//! have to wait through. It no longer needs to be joined for `stop` to
//! return quickly, now that its socket closes under it.
//!
//! `forget` has the same shape of problem for one device, without touching
//! every connection the way `stop` does, so it still solves it the old
//! way: it switches the served filesystem off instead of closing a socket.
//! Every operation on that connection is refused from that moment. The
//! switch is put in place as soon as the handshake proves who is calling,
//! before names are exchanged, so a peer that delays its `hello` is still
//! within reach of `forget`.

uniffi::setup_scaffolding!();

mod access;
mod auto_copy;
mod batch;
mod dav;
mod engine;
pub mod errors;
mod folder;
mod guard;
mod held;
mod networks;
mod notify;
mod push;
mod record;
mod state;
mod transfer;

pub use engine::{Engine, FERRY_PHONE_PORT, generate_key, welcomes_inbound};
pub use networks::wifi_presence_rule;

use std::fmt;

/// One named, shared folder, as the peer sees it.
///
/// `docs/engine-contract.md`, batch C, item 15.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Root {
    /// What the peer sees as this root's first path segment, such as
    /// `"Desktop"`.
    pub name: String,
    /// Where this root lives on disk. Must be an existing directory.
    pub path: String,
    /// False for a root the peer may read but not write.
    pub writable: bool,
}

/// What kind of device this is, or a peer said it is in `hello`.
///
/// `docs/engine-contract.md`, batch C, item 11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DeviceKind {
    /// An Android phone.
    Phone,
    /// A Mac.
    Mac,
}

/// What the app tells the engine at construction.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Config {
    /// Where the engine keeps its own files: paired devices and transfer
    /// records. Must exist and be private to the app.
    pub data_dir: String,
    /// The named folders served to paired devices. At least one.
    pub shared_roots: Vec<Root>,
    /// Where a pulled file lands. Never served to a peer by being here.
    pub download_dir: String,
    /// The name sent in `hello`. At most 64 bytes. Defaults to the model.
    pub display_name: String,
    /// The port to listen on. Zero means any free port.
    pub listen_port: u16,
    /// This device's long-lived key. The app loaded it from secure storage.
    pub key: KeyPair,
    /// What kind of device this is. Sent in `hello`.
    pub kind: DeviceKind,
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

/// Everything this engine currently is, as one call rather than several
/// facts held twice.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Status {
    /// True while this device advertises and accepts connections.
    pub reachable: bool,
    /// The port the listener is bound to. Zero before `start` has run.
    pub listen_port: u16,
    /// Whether `adb` was found when the engine started.
    pub adb_present: bool,
    /// The Wi-Fi network name the app last set. `None` when unknown.
    pub network: Option<String>,
    /// True while this device advertises, browses, and accepts over Wi-Fi.
    pub wifi_presence: bool,
}

/// Where the `WebDAV` bridge for one device answers, and the credentials to
/// mount it.
///
/// `docs/engine-contract.md`, item 6. Loopback only: `url` is always
/// `"http://127.0.0.1:<port>/"`.
#[derive(Clone, uniffi::Record)]
pub struct MountEndpoint {
    /// `"http://127.0.0.1:<port>/"`.
    pub url: String,
    /// The Basic auth user name. Fixed; only the password is secret.
    pub user: String,
    /// Random per `mount_start`. Never shown on screen.
    pub password: String,
}

impl fmt::Debug for MountEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The password must never reach a log, the same rule `KeyPair`
        // follows for the private half of a key.
        f.debug_struct("MountEndpoint")
            .field("url", &self.url)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
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
    /// Every transport this device could currently be reached through,
    /// `Usb` first. `reachable_via`, when it is `Some`, is always in this
    /// list.
    pub available_transports: Vec<Transport>,
    /// What kind of device it said it was, in `hello` at pairing time.
    pub kind: DeviceKind,
    /// Where the peer's roots are mounted on this device, or `None` while
    /// no bridge is serving it or the app has not reported a path yet.
    ///
    /// `docs/engine-contract.md`, item 6. Replaces `Status.mount`, item 1:
    /// one fact, one place, and the fact is per device.
    pub mount_path: Option<String>,
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

/// Which way a transfer moves a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Direction {
    /// This device fetched the file from the peer.
    Pull,
    /// This device sent the file to the peer.
    Push,
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
    /// When `pull` created this transfer.
    pub started_unix_secs: i64,
    /// When the state last became `Done` or `Failed`. Cleared by `retry`.
    pub ended_unix_secs: Option<i64>,
    /// Which way this transfer moves the file.
    pub direction: Direction,
    /// Bytes per second, measured over this transfer's own bytes across the
    /// last two seconds. `None` unless the state is `Active`.
    pub speed_bytes_per_sec: Option<u64>,
    /// The chunk count of the file, known once the size is.
    pub chunks_total: u32,
    /// How many chunks have a verified hash so far.
    pub chunks_verified: u32,
    /// Which batch this transfer belongs to, if `pull_folder` created it.
    /// `None` for a transfer a single `pull` created.
    pub batch_id: Option<String>,
}

/// Why a batch exists.
///
/// `docs/engine-contract.md`, batch D, item 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Origin {
    /// A person asked for it.
    Manual,
    /// Ferry decided, under item 14's rule. `auto_copy.rs` is the only
    /// place that ever builds one.
    Automatic,
}

/// One group of transfers made by one [`Engine::pull_folder`] call, as the
/// Files section shows it.
///
/// `docs/engine-contract.md`, batch D, item 2. Only what does not change
/// once the batch is made is stored on disk. Every other field here —
/// `files_done`, the byte counts, `state`, `speed_bytes_per_sec`,
/// `ended_unix_secs`, `transport`, and `error` — is computed fresh from the
/// transfers named on the batch, every time the app asks.
#[derive(Debug, Clone, uniffi::Record)]
pub struct BatchInfo {
    /// Stable for the life of the batch, across restarts.
    pub id: String,
    /// Which device the files come from.
    pub device_key_hex: String,
    /// The remote path as given: `"Internal storage/DCIM/Camera"`.
    pub label: String,
    /// How many files this batch covers.
    pub files_total: u32,
    /// How many of those files are `Done`.
    pub files_done: u32,
    /// The sum of `bytes_total` over its transfers.
    pub bytes_total: u64,
    /// The sum of `bytes_done` over its transfers.
    pub bytes_done: u64,
    /// The worst state among its transfers: `Failed`, then `Paused`, then
    /// `Active`, then `Queued`, then `Done`. A batch with no files is `Done`.
    pub state: TransferState,
    /// Which way every transfer in this batch moves its file.
    pub direction: Direction,
    /// Why this batch exists.
    pub origin: Origin,
    /// The sum over its active transfers. `None` while none are active.
    pub speed_bytes_per_sec: Option<u64>,
    /// When `pull_folder` created this batch.
    pub started_unix_secs: i64,
    /// The latest end time among its transfers, once none is `Queued`,
    /// `Active`, or `Paused`. `None` while one still is. A `retry` clears
    /// this the same way it clears that one transfer's own end time. A
    /// batch with no files carries `started_unix_secs` here.
    pub ended_unix_secs: Option<i64>,
    /// The transport of any transfer in this batch that is `Active`.
    /// `None` while none are.
    pub transport: Option<Transport>,
    /// The error of the first `Failed` transfer in this batch, in id order.
    /// `None` unless `state` is `Failed`.
    pub error: Option<FerryError>,
}

/// Job 7: whether this device copies a paired device's camera folder to
/// itself on its own, and what its last run did.
///
/// `docs/engine-contract.md`, item 14. `source` and `destination` are never
/// stored: `source` is learned fresh from the peer's own roots each time a
/// run starts, and is the placeholder `"DCIM"` until the first run has
/// learned it; `destination` is always computed from the current download
/// folder. [`Engine::auto_copy`] always answers, even for a device that is
/// not paired.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AutoCopy {
    /// Which device this describes.
    pub device_key_hex: String,
    /// Whether the switch is on.
    pub enabled: bool,
    /// The peer folder watched, root-relative: `"Internal storage/DCIM"`.
    pub source: String,
    /// Where copies land: `"<download_dir>/DCIM"`.
    pub destination: String,
    /// When the last run ended, if one ever has.
    pub last_run_unix_secs: Option<i64>,
    /// How many files the last run copied, zero when it found nothing new.
    /// `Some` exactly when `last_run_unix_secs` is.
    pub last_run_files: Option<u32>,
    /// Whether a run is moving files right now.
    ///
    /// Derived, not stored: true while a batch with `Origin::Automatic` for
    /// this device is not `Done` or `Failed`. A run that finds nothing new
    /// records a run and makes no batch, so `running` is only ever true
    /// while files are actually moving, never for the run's own listing and
    /// skip-check.
    pub running: bool,
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

/// How two devices pair.
///
/// `docs/engine-contract.md` item 12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PairingMethod {
    /// A six digit code, shown on both screens and confirmed on both.
    Code,
    /// A code scanned from the other device's screen. `Offering` draws it,
    /// `offer_scanned` reads it.
    Qr,
}

/// What a Mac draws as a QR code while `PairingState::Offering`.
///
/// `docs/engine-contract.md` item 12.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PairingOffer {
    /// ASCII: `"FERRY1:"` then base64url of version(1), the Mac's static
    /// public key(32), expiry(8), nonce(16), then addresses as count(1) and
    /// ip(16 or 4 with a tag) and port(2) each. Drawn as a QR code. See
    /// `ferry_core::offer::Offer`, which this is encoded from.
    pub payload: Vec<u8>,
    /// When this offer stops accepting a scan.
    pub expires_unix_secs: i64,
}

/// Where pairing is.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum PairingState {
    /// Not pairing.
    Idle,
    /// Looking for a device, or on the phone, waiting for a Mac. Code
    /// method.
    Waiting {
        /// When this pairing attempt gives up.
        expires_unix_secs: i64,
    },
    /// The Mac has candidates to pick from. Code method.
    Found {
        /// What can be picked.
        candidates: Vec<PairingCandidate>,
        /// When this pairing attempt gives up.
        expires_unix_secs: i64,
    },
    /// Both screens show the code. Code method.
    Code {
        /// Six digits, zero padded.
        code: String,
        /// When this pairing attempt gives up.
        expires_unix_secs: i64,
    },
    /// The Mac is showing a QR code, and nobody has scanned it yet. QR
    /// method.
    Offering {
        /// What to draw. `offer.expires_unix_secs` is this state's deadline.
        offer: PairingOffer,
    },
    /// The Mac read a scan's hello and is waiting for `confirm_pairing`. QR
    /// method. The phone never shows this: it asks no question of its own.
    Requested {
        /// The scanning phone's name, from its hello.
        name: String,
        /// The scanning device's kind, from its hello. Shown as the
        /// device's own icon, rather than assuming every scan is a phone.
        kind: DeviceKind,
        /// How the phone reached this Mac. Always `Wifi`: QR pairing only
        /// dials the Wi-Fi addresses in the offer.
        transport: Transport,
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

/// One kind of file operation the access log records.
///
/// `docs/engine-contract.md`, batch E, item 13. `set_mtime` has no member
/// here: it is never logged, because it always follows a write that already
/// is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AccessVerb {
    /// A folder listing. One entry covers every page of it.
    List,
    /// A single file or folder's metadata.
    Stat,
    /// Bytes read from a file.
    Read,
    /// Bytes written to a file.
    Write,
    /// A file shortened or extended to a given length.
    Truncate,
    /// A file or folder renamed.
    Rename,
    /// A folder created.
    Mkdir,
    /// A file or folder removed.
    Delete,
}

/// Who performed an access log entry's operation.
///
/// `docs/engine-contract.md`, batch E, item 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Actor {
    /// The peer, on this device's files.
    Peer,
    /// This device, on the peer's files.
    This,
}

/// One access log entry, as [`Engine::access_log`] returns it.
///
/// `docs/engine-contract.md`, batch E, item 13.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AccessEntry {
    /// `"<day>-<sequence>"`. Stable across restarts.
    pub id: String,
    /// The paired device this entry is about, as 64 lowercase hex
    /// characters.
    pub device_key_hex: String,
    /// Who performed the operation.
    pub actor: Actor,
    /// Which kind of operation.
    pub verb: AccessVerb,
    /// Root-relative, beginning with the root name: `"Desktop/Q3 notes.md"`.
    pub path: String,
    /// Bytes moved, for a read or a write.
    pub bytes: Option<u64>,
    /// For a list, how many entries were returned.
    pub entries: Option<u32>,
    /// For a folder copy, how many files it covered.
    pub files: Option<u32>,
    /// When the operation this entry describes first happened.
    pub at_unix_secs: i64,
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
    /// An access log entry became final. At most once every 250
    /// milliseconds. The app then calls [`Engine::access_log`].
    fn access_log_changed(&self);
}
