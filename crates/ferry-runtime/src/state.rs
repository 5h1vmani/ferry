//! What the engine knows, and the small helpers around it.
//!
//! Everything mutable lives in [`State`], behind one mutex. No thread holds
//! that mutex across a read or a write on a socket. A thread copies what it
//! needs, drops the lock, does the input and output, then takes the lock
//! again to write the result down.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_core::chunk::ChunkSize;
use ferry_core::noise::{PublicKey, QR_NONCE_LEN, SecureStream};
use ferry_core::path::RemotePath;
use ferry_core::peers::{DeviceKind as CoreDeviceKind, PeerStore};
use ferry_core::tcp::PairedConnection;

use crate::networks::TrustedNetworks;
use crate::{
    DeviceInfo, DeviceKind, Direction, FerryError, Origin, PairingCandidate, PairingState,
    TransferState, Transport,
};

/// How long a Wi-Fi success still counts in `available_transports`, once no
/// further Wi-Fi connection has succeeded since.
///
/// This reuses the pairing timeout's value, the nearest existing "how stale
/// is too stale" window in this engine.
const WIFI_SUCCESS_LIFETIME_SECS: i64 = 120;

/// Take a mutex without ever panicking.
///
/// A poisoned mutex means some other thread panicked while holding it. The
/// data is still there, and refusing to look at it would turn one fault into
/// a dead engine. So the guard is taken either way.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The current time in seconds since the Unix epoch.
///
/// A clock set before 1970 gives zero rather than an error. The value is only
/// ever shown, so a wrong date is better than a failed pairing.
pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// A public key as 64 lowercase hex characters. This is the device identity.
pub(crate) fn hex_of(key: &PublicKey) -> String {
    let mut out = String::with_capacity(64);
    for byte in key.as_bytes().iter().copied() {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

/// One hex digit for a value below sixteen.
///
/// A value that is not below sixteen cannot reach here, because both callers
/// mask it first. Zero is the answer if one ever did.
fn hex_digit(value: u8) -> char {
    char::from_digit(u32::from(value), 16).unwrap_or('0')
}

/// Read a public key back from 64 lowercase hex characters.
///
/// Returns `None` for anything that is not exactly 64 hex characters.
pub(crate) fn key_from_hex(text: &str) -> Option<PublicKey> {
    if text.len() != 64 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&bytes[index * 2..index * 2 + 2]).ok()?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(PublicKey(out))
}

/// What the engine knows about one paired device right now.
///
/// None of this is stored. It is rebuilt every run from connections that
/// succeed and connections that end.
#[derive(Debug, Default)]
pub(crate) struct DeviceLive {
    /// How the device is reachable at this moment, if at all.
    pub(crate) reachable_via: Option<Transport>,
    /// When it was last reachable.
    pub(crate) last_seen_unix_secs: Option<i64>,
    /// The address that last worked, so a reconnect needs no discovery.
    ///
    /// This is kept here and not in `PeerStore`, because the stored format
    /// has no field for it and this build does not change that format.
    pub(crate) last_addr: Option<SocketAddr>,
    /// The local port of an `adb` forward that reached this device.
    pub(crate) usb_port: Option<u16>,
    /// When a Wi-Fi connection to this device last succeeded.
    ///
    /// Set only in [`crate::engine::mark_reachable`], and only when `via` is
    /// [`Transport::Wifi`]. `last_seen_unix_secs` is no evidence of Wi-Fi on
    /// its own: it is also touched by a USB connection, so a cable-only
    /// phone must not be able to claim Wi-Fi through it.
    pub(crate) last_wifi_success_unix_secs: Option<i64>,
    /// Bytes moved with this device in the last second.
    pub(crate) speed_bytes_per_sec: Option<u64>,
    /// Switches held by connections now serving this device.
    ///
    /// `forget` sets every one of them to false. A served connection then
    /// refuses every operation, even though its socket is still open.
    pub(crate) serving: Vec<Arc<AtomicBool>>,
    /// Where the app reported this device's `WebDAV` bridge is mounted, or
    /// `None` while it is not mounted.
    ///
    /// `docs/engine-contract.md`, item 6. Set by `Engine::set_mount_path`,
    /// not persisted: an OS mount does not survive an engine restart
    /// either, so nothing here needs to.
    pub(crate) mount_path: Option<String>,
    /// This device's first shared root, as `auto_copy.rs`'s run last learned
    /// it. `docs/engine-contract.md`, item 14: `AutoCopy.source` is never
    /// stored, so this is only ever a cache for `Engine::auto_copy` to read
    /// without a network call, cleared like everything else in `DeviceLive`
    /// by an engine restart.
    pub(crate) auto_copy_source_root: Option<String>,
}

/// One device that could be paired, with the address to dial it on.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// What the app sees.
    pub(crate) shown: PairingCandidate,
    /// Where to dial.
    pub(crate) addr: SocketAddr,
}

/// The connection a pairing is holding while the two codes are compared.
///
/// It sits here, unread, until `confirm_pairing` takes it out. Holding it is
/// the whole reason pairing has a Code state.
pub(crate) struct HeldPairing {
    /// The paired channel, the peer key, and the code.
    pub(crate) connection: PairedConnection,
    /// The address of the other device.
    pub(crate) addr: SocketAddr,
    /// True when this side accepted the connection rather than dialing it.
    ///
    /// The side that accepted keeps serving on the stream afterwards. The
    /// side that dialed lets it go, because two servers on one stream would
    /// both wait for the other to speak.
    pub(crate) accepted: bool,
}

/// A QR pairing handshake that finished and is waiting for `confirm_pairing`.
///
/// The QR method's counterpart to [`HeldPairing`]. There is no code to
/// compare, so nothing plays the part [`HeldPairing::accepted`] plays for the
/// code method: the Mac is always the side that accepted this connection,
/// per `docs/engine-contract.md` item 12, so `finish_pairing` is always told
/// `accepted: true` for one of these.
///
/// `name` and `kind` are message one's hello, kept so `finish_pairing` can
/// check its own, later hello exchange against them: the two must agree on
/// who this is, or the pairing fails the same way a bad hello anywhere else
/// does.
pub(crate) struct RequestedPairing {
    /// The encrypted channel, ready for `finish_pairing`'s hello exchange.
    pub(crate) stream: SecureStream,
    /// The phone's static public key, from the handshake.
    pub(crate) peer: PublicKey,
    /// The address the phone dialed from.
    pub(crate) addr: SocketAddr,
    /// The name message one's hello carried.
    pub(crate) name: String,
    /// The kind message one's hello carried.
    pub(crate) kind: CoreDeviceKind,
}

/// Where pairing is, and what it is holding.
pub(crate) struct Pairing {
    /// The state last reported to the app.
    pub(crate) shown: PairingState,
    /// When pairing gives up, if it is running. Shared by both methods.
    pub(crate) deadline: Option<Instant>,
    /// The same deadline, in Unix seconds, for the wire. Set and cleared
    /// together with `deadline`.
    pub(crate) deadline_unix_secs: Option<i64>,
    /// The connection waiting for a confirm. Code method only.
    pub(crate) held: Option<HeldPairing>,
    /// True while a chosen candidate is being dialed. Code method only.
    ///
    /// A handshake takes a moment, and until it finishes nothing is held. So
    /// without this a second tap on a candidate would start a second
    /// handshake, and the two would show two different codes.
    pub(crate) dialing: bool,
    /// Candidates found so far, by identifier. Code method only.
    pub(crate) candidates: BTreeMap<String, Candidate>,
    /// The nonce of the offer currently shown as `PairingState::Offering`, or
    /// `None` while not offering. QR method only.
    ///
    /// Cleared the moment a scan's nonce matches it, before `Requested` is
    /// shown, so the nonce is single use: a second scan of the same code,
    /// even a genuine one racing the first, finds nothing to match and is
    /// refused by [`ferry_core::noise::NoiseError::UnknownOffer`].
    pub(crate) offer_nonce: Option<[u8; QR_NONCE_LEN]>,
    /// The QR handshake waiting for a confirm. QR method only.
    pub(crate) requested: Option<RequestedPairing>,
}

impl Pairing {
    /// A pairing that is not running.
    pub(crate) fn idle() -> Self {
        Self {
            shown: PairingState::Idle,
            deadline: None,
            deadline_unix_secs: None,
            held: None,
            dialing: false,
            candidates: BTreeMap::new(),
            offer_nonce: None,
            requested: None,
        }
    }

    /// True while the engine is looking for a device or listing candidates,
    /// for the code method.
    ///
    /// An inbound connection is offered the `XX` pairing handshake only in
    /// these two states, and only while nothing is held and nothing is
    /// being dialed.
    pub(crate) fn is_open_to_pairing(&self) -> bool {
        self.held.is_none()
            && !self.dialing
            && matches!(
                self.shown,
                PairingState::Waiting { .. } | PairingState::Found { .. }
            )
    }

    /// True while showing a QR offer nobody has scanned yet.
    ///
    /// An inbound connection is offered the `IK` pairing handshake only in
    /// this state, and only while nothing is already `Requested`.
    pub(crate) fn is_offering(&self) -> bool {
        self.requested.is_none() && matches!(self.shown, PairingState::Offering { .. })
    }

    /// True while either method is open to a new inbound handshake attempt.
    /// What `accept_loop`'s welcome check adds to `reachable`.
    pub(crate) fn accepts_inbound(&self) -> bool {
        self.is_open_to_pairing() || self.is_offering()
    }

    /// True while pairing is running and has not reached an end state,
    /// under either method.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self.shown,
            PairingState::Waiting { .. }
                | PairingState::Found { .. }
                | PairingState::Code { .. }
                | PairingState::Offering { .. }
                | PairingState::Requested { .. }
        )
    }
}

/// One `adb` forward this engine opened.
#[derive(Debug, Clone)]
pub(crate) struct UsbForward {
    /// The device serial the forward reaches.
    pub(crate) serial: String,
    /// The local port on this machine.
    pub(crate) local_port: u16,
}

/// One transfer, as the engine tracks it.
///
/// The `Transfer` record in `ferry-core` holds the manifest and the paths.
/// This holds everything the screen needs and everything the retry loop
/// needs.
#[derive(Debug, Clone)]
pub(crate) struct TransferRow {
    /// The identifier the app was given. It also names the record file.
    pub(crate) id: String,
    /// Which device the file comes from.
    pub(crate) device_key_hex: String,
    /// The file's name, for display.
    pub(crate) file_name: String,
    /// Where the file lives on the other device.
    pub(crate) source: RemotePath,
    /// Where the file belongs here.
    pub(crate) destination: RemotePath,
    /// Size in bytes, once a `stat` has answered.
    pub(crate) bytes_total: u64,
    /// Bytes verified in place.
    pub(crate) bytes_done: u64,
    /// Where the transfer is.
    pub(crate) state: TransferState,
    /// Which transport is carrying it, while it is active.
    pub(crate) transport: Option<Transport>,
    /// Why it paused or failed.
    pub(crate) error: Option<FerryError>,
    /// The size the source had when the first pass started.
    ///
    /// A different size on a later attempt means the file changed, so the
    /// first pass starts again from zero.
    pub(crate) source_size: Option<u64>,
    /// The modified time the source had when the first pass started.
    pub(crate) source_mtime: Option<i64>,
    /// True once this transfer is queued for a worker or running on one.
    pub(crate) running: bool,
    /// When the next attempt may start, while the transfer is waiting.
    ///
    /// A worker is a shared thing, so a transfer that is waiting out its
    /// backoff sits in the queue with a time on it instead of holding a
    /// worker asleep.
    pub(crate) attempt_after: Option<Instant>,
    /// How long the next wait after a failed attempt is.
    pub(crate) backoff: Duration,
    /// When `pull` created this row.
    pub(crate) started_unix_secs: i64,
    /// When the state last became `Done` or `Failed`. Cleared by `retry`.
    pub(crate) ended_unix_secs: Option<i64>,
    /// Which way this transfer moves the file. Always `Pull` until item 5.
    pub(crate) direction: Direction,
    /// This transfer's own bytes per second, over the last two seconds.
    /// Meaningless once the state is not `Active`; `info` hides it then.
    pub(crate) speed_bytes_per_sec: Option<u64>,
    /// Which batch this transfer belongs to, if `pull_folder` created it.
    pub(crate) batch_id: Option<String>,
    /// The chunk size this transfer's first pass used, or will use once it
    /// runs.
    ///
    /// Set once at row creation to whichever chunk size the engine's own
    /// setting names at that moment, a placeholder until `transfer::
    /// first_pass` fetches the peer's manifest and overwrites it with the
    /// manifest's own chunk size, the one this transfer actually verifies
    /// against from then on. `set_chunk_size` only decides the next first
    /// pass, so a later change to it must not retroactively change what
    /// `chunks_total` reports for a transfer already under way, nor for one
    /// loaded back from a record written under an earlier setting. See
    /// `chunk_counts`.
    pub(crate) chunk_size: ChunkSize,
}

/// The chunk count a size implies, and how many of those chunks are
/// verified, given how many bytes are verified in place.
///
/// Nothing new is stored for this. Both numbers come from `bytes_total`,
/// `bytes_done`, and the chunk size the transfer's own row carries.
fn chunk_counts(bytes_total: u64, bytes_done: u64, chunk_size: ChunkSize) -> (u32, u32) {
    let total = u32::try_from(bytes_total.div_ceil(chunk_size.as_u64())).unwrap_or(u32::MAX);
    let verified = if bytes_total > 0 && bytes_done >= bytes_total {
        total
    } else {
        u32::try_from(bytes_done / chunk_size.as_u64()).unwrap_or(u32::MAX)
    };
    (total, verified)
}

impl TransferRow {
    /// The view of this row that crosses the boundary.
    pub(crate) fn info(&self) -> crate::TransferInfo {
        let (chunks_total, chunks_verified) =
            chunk_counts(self.bytes_total, self.bytes_done, self.chunk_size);
        crate::TransferInfo {
            id: self.id.clone(),
            device_key_hex: self.device_key_hex.clone(),
            file_name: self.file_name.clone(),
            bytes_total: self.bytes_total,
            bytes_done: self.bytes_done,
            state: self.state,
            transport: self.transport,
            error: self.error.clone(),
            started_unix_secs: self.started_unix_secs,
            ended_unix_secs: self.ended_unix_secs,
            direction: self.direction,
            speed_bytes_per_sec: (self.state == TransferState::Active)
                .then_some(self.speed_bytes_per_sec)
                .flatten(),
            chunks_total,
            chunks_verified,
            batch_id: self.batch_id.clone(),
        }
    }
}

/// One batch, as the engine tracks it.
///
/// Only what does not change once the batch is made, plus the done floor
/// below, lives here: see `batch.rs` for how this is stored. Every other
/// aggregate — bytes total, state, speed, and ended — is computed fresh from
/// the live transfer rows every time the app asks. See [`BatchRow::info`].
#[derive(Debug, Clone)]
pub(crate) struct BatchRow {
    /// The identifier the app was given. It also names the record file, and
    /// carries the device key as the text before its first `-`, the same
    /// way a transfer id does.
    pub(crate) id: String,
    /// Which device the files come from.
    pub(crate) device_key_hex: String,
    /// The remote path as given to `pull_folder`.
    pub(crate) label: String,
    /// Which way every transfer in this batch moves its file.
    pub(crate) direction: Direction,
    /// Why this batch exists.
    pub(crate) origin: Origin,
    /// When `pull_folder` created this batch.
    pub(crate) started_unix_secs: i64,
    /// The transfer ids this batch covers, in the order they were queued.
    pub(crate) transfer_ids: Vec<String>,
    /// How many of `transfer_ids` have reached `Done`, as last written to
    /// the batch record. A floor `info` reports at least: a `Done`
    /// transfer's own row does not survive a restart, so the live count
    /// alone would undercount, or lose, everything a batch finished before
    /// one. Raised by `transfer::finish` each time a transfer in this batch
    /// reaches `Done`; never lowered.
    pub(crate) done_files: u32,
    /// The sum of `bytes_total` over the transfers `done_files` counts.
    pub(crate) done_bytes: u64,
}

impl BatchRow {
    /// The view of this batch that crosses the boundary.
    ///
    /// `files_total` is the length of `transfer_ids` itself: it is a stored
    /// fact, not an aggregate, so it never shrinks when a finished
    /// transfer's row does not survive a restart. `files_done` and
    /// `bytes_done` are the larger of `done_files`/`done_bytes` and what the
    /// transfer ids that currently name a row in `transfers` add up to; see
    /// the field documentation on `done_files`. Every other aggregate field
    /// is computed only from those live rows.
    pub(crate) fn info(&self, transfers: &BTreeMap<String, TransferRow>) -> crate::BatchInfo {
        let rows: Vec<&TransferRow> = self
            .transfer_ids
            .iter()
            .filter_map(|id| transfers.get(id))
            .collect();
        let files_total = u32::try_from(self.transfer_ids.len()).unwrap_or(u32::MAX);
        let live_files_done = u32::try_from(
            rows.iter()
                .filter(|row| row.state == TransferState::Done)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let files_done = self.done_files.max(live_files_done);
        let live_bytes_done: u64 = rows.iter().map(|row| row.bytes_done).sum();
        let bytes_done = self.done_bytes.max(live_bytes_done);
        let bytes_total = rows.iter().map(|row| row.bytes_total).sum();
        let state = worst_batch_state(&rows);
        let speed_bytes_per_sec = rows
            .iter()
            .any(|row| row.state == TransferState::Active)
            .then(|| {
                rows.iter()
                    .filter(|row| row.state == TransferState::Active)
                    .filter_map(|row| row.speed_bytes_per_sec)
                    .sum()
            });
        let still_moving = rows.iter().any(|row| {
            matches!(
                row.state,
                TransferState::Queued | TransferState::Active | TransferState::Paused
            )
        });
        let ended_unix_secs = if still_moving {
            None
        } else {
            rows.iter()
                .filter_map(|row| row.ended_unix_secs)
                .max()
                .or(Some(self.started_unix_secs))
        };
        // Item 1's additions: the transport of any row that is Active, and
        // the first Failed row's error, in id order rather than queued
        // order, so which one is reported does not depend on how the batch
        // happened to be built.
        let transport = rows
            .iter()
            .find(|row| row.state == TransferState::Active)
            .and_then(|row| row.transport);
        let error = rows
            .iter()
            .filter(|row| row.state == TransferState::Failed)
            .min_by_key(|row| &row.id)
            .and_then(|row| row.error.clone());
        crate::BatchInfo {
            id: self.id.clone(),
            device_key_hex: self.device_key_hex.clone(),
            label: self.label.clone(),
            files_total,
            files_done,
            bytes_total,
            bytes_done,
            state,
            direction: self.direction,
            origin: self.origin,
            speed_bytes_per_sec,
            started_unix_secs: self.started_unix_secs,
            ended_unix_secs,
            transport,
            error,
        }
    }
}

/// The worst state among a batch's transfers: `Failed`, then `Paused`, then
/// `Active`, then `Queued`, then `Done`. No transfers at all is `Done`, the
/// same answer a batch with zero files gives for the same reason.
fn worst_batch_state(rows: &[&TransferRow]) -> TransferState {
    for candidate in [
        TransferState::Failed,
        TransferState::Paused,
        TransferState::Active,
        TransferState::Queued,
    ] {
        if rows.iter().any(|row| row.state == candidate) {
            return candidate;
        }
    }
    TransferState::Done
}

/// Everything the engine knows, behind one mutex.
pub(crate) struct State {
    /// True once `start` finished.
    pub(crate) started: bool,
    /// True once `stop` began. Nothing new is accepted after that.
    pub(crate) stopped: bool,
    /// True while this device advertises and accepts connections.
    pub(crate) reachable: bool,
    /// The address the listener is bound to.
    pub(crate) listen_addr: Option<SocketAddr>,
    /// The paired devices, as stored on disk.
    pub(crate) peers: PeerStore,
    /// What is known about each paired device right now, by key hex.
    pub(crate) live: BTreeMap<String, DeviceLive>,
    /// The one pairing that may be running.
    pub(crate) pairing: Pairing,
    /// Every transfer, by identifier.
    pub(crate) transfers: BTreeMap<String, TransferRow>,
    /// Every batch, by identifier.
    pub(crate) batches: BTreeMap<String, BatchRow>,
    /// The transfers waiting for a worker, oldest first.
    pub(crate) queue: VecDeque<String>,
    /// How many transfer workers are running.
    pub(crate) workers: usize,
    /// The `adb` forwards this engine opened.
    pub(crate) forwards: Vec<UsbForward>,
    /// Addresses discovery has seen, newest first.
    ///
    /// An mDNS record carries no key, so an address here is only a place to
    /// try. A wrong guess fails the handshake and costs nothing.
    pub(crate) discovered: Vec<SocketAddr>,
    /// The Wi-Fi network name the app last reported, or `None` while it is
    /// unknown: Wi-Fi off, the location permission refused, or the name
    /// unreadable. `docs/engine-contract.md` item 18.
    pub(crate) network: Option<String>,
    /// The Wi-Fi networks this device is willing to be present on, as
    /// stored in `data_dir/networks`. An empty list trusts every network.
    pub(crate) trusted: TrustedNetworks,
}

impl State {
    /// A fresh state around a loaded peer store and a loaded trusted
    /// network list.
    pub(crate) fn new(peers: PeerStore, trusted: TrustedNetworks) -> Self {
        Self {
            started: false,
            stopped: false,
            reachable: false,
            listen_addr: None,
            peers,
            live: BTreeMap::new(),
            pairing: Pairing::idle(),
            transfers: BTreeMap::new(),
            batches: BTreeMap::new(),
            queue: VecDeque::new(),
            workers: 0,
            forwards: Vec::new(),
            discovered: Vec::new(),
            network: None,
            trusted,
        }
    }

    /// The live record for one device, created if it is not there yet.
    pub(crate) fn live_mut(&mut self, key_hex: &str) -> &mut DeviceLive {
        self.live.entry(key_hex.to_owned()).or_default()
    }

    /// The device list, as the app shows it.
    pub(crate) fn devices(&self) -> Vec<DeviceInfo> {
        let now = now_unix_secs();
        self.peers
            .all()
            .into_iter()
            .map(|peer| {
                let key_hex = hex_of(&peer.key);
                let live = self.live.get(&key_hex);
                DeviceInfo {
                    key_hex,
                    name: peer.name,
                    paired_unix_secs: peer.paired_unix_secs,
                    reachable_via: live.and_then(|l| l.reachable_via),
                    speed_bytes_per_sec: live.and_then(|l| l.speed_bytes_per_sec),
                    last_seen_unix_secs: live.and_then(|l| l.last_seen_unix_secs),
                    available_transports: available_transports(live, now),
                    kind: DeviceKind::from(peer.kind),
                    mount_path: live.and_then(|l| l.mount_path.clone()),
                }
            })
            .collect()
    }

    /// One device, as the app shows it, or `None` when it is not paired.
    pub(crate) fn device(&self, key_hex: &str) -> Option<DeviceInfo> {
        self.devices().into_iter().find(|d| d.key_hex == key_hex)
    }

    /// The candidate list, in identifier order.
    pub(crate) fn candidate_list(&self) -> Vec<PairingCandidate> {
        self.pairing
            .candidates
            .values()
            .map(|c| c.shown.clone())
            .collect()
    }
}

/// Every transport one device could currently be reached through, `Usb`
/// first.
///
/// `Usb` holds when `usb_port` is `Some`: an `adb` tunnel is a cable, and
/// nothing else opens one. `Wifi` holds when a Wi-Fi connection succeeded
/// within [`WIFI_SUCCESS_LIFETIME_SECS`], or when `reachable_via` is `Wifi`
/// right now. Either transport is folded in through `reachable_via` as well,
/// so a transport this device is serving on right now is never missing from
/// its own list.
///
/// `last_seen_unix_secs` is deliberately not used here: it updates on every
/// connection regardless of transport, so it cannot tell a Wi-Fi sighting
/// from a USB one.
/// Clear `usb_port` for every live device whose forward is one of
/// `gone_ports`.
///
/// Called from `engine::poll_adb_once` once it knows which forwards no
/// longer name a plugged-in device. `usb_port` names the forward a device
/// was last reached through, and `available_transports` reads only
/// `is_some()` off it, so a stale port left behind would keep `Usb` listed
/// for a device whose cable is gone.
pub(crate) fn clear_gone_usb_forwards(live: &mut BTreeMap<String, DeviceLive>, gone_ports: &[u16]) {
    for device in live.values_mut() {
        if device
            .usb_port
            .is_some_and(|port| gone_ports.contains(&port))
        {
            device.usb_port = None;
        }
    }
}

fn available_transports(live: Option<&DeviceLive>, now: i64) -> Vec<Transport> {
    let Some(live) = live else {
        return Vec::new();
    };
    let mut transports = Vec::new();
    if live.usb_port.is_some() || live.reachable_via == Some(Transport::Usb) {
        transports.push(Transport::Usb);
    }
    let wifi_succeeded_recently = live
        .last_wifi_success_unix_secs
        .is_some_and(|seen| now.saturating_sub(seen) <= WIFI_SUCCESS_LIFETIME_SECS);
    if wifi_succeeded_recently || live.reachable_via == Some(Transport::Wifi) {
        transports.push(Transport::Wifi);
    }
    transports
}

#[cfg(test)]
mod tests {
    use super::{
        BatchRow, DeviceLive, TransferRow, Transport, WIFI_SUCCESS_LIFETIME_SECS,
        available_transports, clear_gone_usb_forwards, worst_batch_state,
    };
    use crate::{Direction, Origin, TransferState};
    use ferry_core::chunk::ChunkSize;
    use ferry_core::path::RemotePath;
    use std::collections::BTreeMap;
    use std::time::Duration;

    #[test]
    fn no_live_record_means_no_transport() {
        assert_eq!(available_transports(None, 1_000), Vec::new());
    }

    /// A row with every field set to something plain, so a test can override
    /// just the ones it cares about.
    fn sample_row(chunk_size: ChunkSize, bytes_total: u64, bytes_done: u64) -> TransferRow {
        TransferRow {
            id: "device-1".to_owned(),
            device_key_hex: "device".to_owned(),
            file_name: "a.bin".to_owned(),
            source: RemotePath::parse("a.bin").expect("a valid path"),
            destination: RemotePath::parse("a.bin").expect("a valid path"),
            bytes_total,
            bytes_done,
            state: TransferState::Paused,
            transport: None,
            error: None,
            source_size: None,
            source_mtime: None,
            running: false,
            attempt_after: None,
            backoff: Duration::from_secs(1),
            started_unix_secs: 0,
            ended_unix_secs: None,
            direction: Direction::Pull,
            speed_bytes_per_sec: None,
            batch_id: None,
            chunk_size,
        }
    }

    #[test]
    fn info_uses_the_rows_own_chunk_size_not_the_engine_default() {
        // Half a mebibyte, deliberately not the engine's own default of one
        // mebibyte (`ChunkSize::one_mebibyte`), the way a row loaded from a
        // record written under an earlier `set_chunk_size` setting would be.
        let half_mebibyte = ChunkSize::new(512 * 1024).expect("a valid chunk size");
        let row = sample_row(half_mebibyte, 2 * 1024 * 1024, 1024 * 1024);

        let info = row.info();

        // At the engine's 1 MiB default this would be 2 chunks total and 1
        // verified. At the row's own 512 KiB it is twice that.
        assert_eq!(
            info.chunks_total, 4,
            "2 MiB at the row's own 512 KiB chunks is 4 chunks, not 2"
        );
        assert_eq!(
            info.chunks_verified, 2,
            "1 MiB done is 2 whole 512 KiB chunks, not 1"
        );
    }

    /// A batch with `transfer_ids` naming every row in `transfers`, in
    /// order, one row per name given.
    fn sample_batch(transfers: &BTreeMap<String, TransferRow>) -> BatchRow {
        BatchRow {
            id: "device-batch".to_owned(),
            device_key_hex: "device".to_owned(),
            label: "Root/Camera".to_owned(),
            direction: Direction::Pull,
            origin: Origin::Manual,
            started_unix_secs: 0,
            transfer_ids: transfers.keys().cloned().collect(),
            done_files: 0,
            done_bytes: 0,
        }
    }

    #[test]
    fn worst_batch_state_prefers_failed_over_done() {
        let done_a = TransferRow {
            state: TransferState::Done,
            ..sample_row(ChunkSize::one_mebibyte(), 10, 10)
        };
        let failed = TransferRow {
            state: TransferState::Failed,
            ..sample_row(ChunkSize::one_mebibyte(), 10, 0)
        };
        let done_b = TransferRow {
            state: TransferState::Done,
            ..sample_row(ChunkSize::one_mebibyte(), 10, 10)
        };
        let rows = [&done_a, &failed, &done_b];
        assert_eq!(
            worst_batch_state(&rows),
            TransferState::Failed,
            "one Failed row among Done ones is still the worst state"
        );
    }

    #[test]
    fn worst_batch_state_prefers_paused_over_active() {
        let active = TransferRow {
            state: TransferState::Active,
            ..sample_row(ChunkSize::one_mebibyte(), 10, 5)
        };
        let paused = TransferRow {
            state: TransferState::Paused,
            ..sample_row(ChunkSize::one_mebibyte(), 10, 5)
        };
        let rows = [&active, &paused];
        assert_eq!(
            worst_batch_state(&rows),
            TransferState::Paused,
            "Paused ranks worse than Active"
        );
    }

    #[test]
    fn batch_info_speed_sums_only_the_active_rows() {
        let mut transfers = BTreeMap::new();
        transfers.insert(
            "device-1".to_owned(),
            TransferRow {
                id: "device-1".to_owned(),
                state: TransferState::Active,
                speed_bytes_per_sec: Some(100),
                ..sample_row(ChunkSize::one_mebibyte(), 10, 5)
            },
        );
        transfers.insert(
            "device-2".to_owned(),
            TransferRow {
                id: "device-2".to_owned(),
                state: TransferState::Active,
                speed_bytes_per_sec: Some(50),
                ..sample_row(ChunkSize::one_mebibyte(), 10, 5)
            },
        );
        transfers.insert(
            "device-3".to_owned(),
            TransferRow {
                id: "device-3".to_owned(),
                state: TransferState::Paused,
                // Set to prove it is excluded, not merely absent.
                speed_bytes_per_sec: Some(9_999),
                ..sample_row(ChunkSize::one_mebibyte(), 10, 5)
            },
        );

        let info = sample_batch(&transfers).info(&transfers);
        assert_eq!(
            info.speed_bytes_per_sec,
            Some(150),
            "only the two Active rows' speeds are summed"
        );
    }

    #[test]
    fn batch_info_speed_is_none_with_no_active_row() {
        let mut transfers = BTreeMap::new();
        transfers.insert(
            "device-1".to_owned(),
            TransferRow {
                id: "device-1".to_owned(),
                state: TransferState::Paused,
                // Set to prove it is excluded, not merely absent.
                speed_bytes_per_sec: Some(100),
                ..sample_row(ChunkSize::one_mebibyte(), 10, 5)
            },
        );

        let info = sample_batch(&transfers).info(&transfers);
        assert_eq!(
            info.speed_bytes_per_sec, None,
            "no row is Active, so there is nothing to report a speed for"
        );
    }

    #[test]
    fn a_gone_forward_drops_usb_from_available_transports() {
        let mut live = BTreeMap::new();
        live.insert(
            "device".to_owned(),
            DeviceLive {
                usb_port: Some(12345),
                ..DeviceLive::default()
            },
        );

        clear_gone_usb_forwards(&mut live, &[12345]);

        assert_eq!(live["device"].usb_port, None, "the stale port is cleared");
        assert_eq!(
            available_transports(live.get("device"), 1_000),
            Vec::new(),
            "available_transports no longer holds Usb once the forward is gone"
        );
    }

    #[test]
    fn a_different_forward_going_away_leaves_this_ports_usb_alone() {
        let mut live = BTreeMap::new();
        live.insert(
            "device".to_owned(),
            DeviceLive {
                usb_port: Some(12345),
                ..DeviceLive::default()
            },
        );

        clear_gone_usb_forwards(&mut live, &[99]);

        assert_eq!(
            available_transports(live.get("device"), 1_000),
            vec![Transport::Usb],
            "a forward for a different port going away does not touch this one"
        );
    }

    #[test]
    fn usb_holds_only_from_the_usb_port() {
        let live = DeviceLive {
            usb_port: Some(12345),
            ..DeviceLive::default()
        };
        assert_eq!(
            available_transports(Some(&live), 1_000),
            vec![Transport::Usb]
        );
    }

    #[test]
    fn wifi_holds_within_the_lifetime_of_the_last_success() {
        let live = DeviceLive {
            last_wifi_success_unix_secs: Some(1_000 - WIFI_SUCCESS_LIFETIME_SECS),
            ..DeviceLive::default()
        };
        assert_eq!(
            available_transports(Some(&live), 1_000),
            vec![Transport::Wifi]
        );
    }

    #[test]
    fn wifi_does_not_hold_once_the_lifetime_has_passed() {
        let live = DeviceLive {
            last_wifi_success_unix_secs: Some(1_000 - WIFI_SUCCESS_LIFETIME_SECS - 1),
            ..DeviceLive::default()
        };
        assert_eq!(available_transports(Some(&live), 1_000), Vec::new());
    }

    #[test]
    fn usb_comes_before_wifi_when_both_hold() {
        let live = DeviceLive {
            usb_port: Some(1),
            last_wifi_success_unix_secs: Some(1_000),
            ..DeviceLive::default()
        };
        assert_eq!(
            available_transports(Some(&live), 1_000),
            vec![Transport::Usb, Transport::Wifi]
        );
    }

    #[test]
    fn reachable_via_is_always_in_the_list_even_past_the_wifi_lifetime() {
        // A connection can be serving right now without
        // `last_wifi_success_unix_secs` having been refreshed yet.
        // `reachable_via` must still show.
        let live = DeviceLive {
            reachable_via: Some(Transport::Wifi),
            last_wifi_success_unix_secs: None,
            ..DeviceLive::default()
        };
        assert_eq!(
            available_transports(Some(&live), 1_000),
            vec![Transport::Wifi]
        );
    }

    #[test]
    fn a_cable_only_phone_does_not_claim_wifi() {
        // The bug this rule replaces: `last_seen_unix_secs` updates over USB
        // too, so a device reached only by cable must not list Wi-Fi just
        // because it was seen recently.
        let live = DeviceLive {
            usb_port: Some(12345),
            reachable_via: Some(Transport::Usb),
            last_seen_unix_secs: Some(1_000),
            last_wifi_success_unix_secs: None,
            ..DeviceLive::default()
        };
        assert_eq!(
            available_transports(Some(&live), 1_000),
            vec![Transport::Usb]
        );
    }
}
