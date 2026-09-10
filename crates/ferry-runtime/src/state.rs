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

use ferry_core::noise::PublicKey;
use ferry_core::path::RemotePath;
use ferry_core::peers::PeerStore;
use ferry_core::tcp::PairedConnection;

use crate::{DeviceInfo, FerryError, PairingCandidate, PairingState, TransferState, Transport};

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
    /// Bytes moved with this device in the last second.
    pub(crate) speed_bytes_per_sec: Option<u64>,
    /// Switches held by connections now serving this device.
    ///
    /// `forget` sets every one of them to false. A served connection then
    /// refuses every operation, even though its socket is still open.
    pub(crate) serving: Vec<Arc<AtomicBool>>,
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

/// Where pairing is, and what it is holding.
pub(crate) struct Pairing {
    /// The state last reported to the app.
    pub(crate) shown: PairingState,
    /// When pairing gives up, if it is running.
    pub(crate) deadline: Option<Instant>,
    /// The connection waiting for a confirm.
    pub(crate) held: Option<HeldPairing>,
    /// True while a chosen candidate is being dialed.
    ///
    /// A handshake takes a moment, and until it finishes nothing is held. So
    /// without this a second tap on a candidate would start a second
    /// handshake, and the two would show two different codes.
    pub(crate) dialing: bool,
    /// Candidates found so far, by identifier.
    pub(crate) candidates: BTreeMap<String, Candidate>,
}

impl Pairing {
    /// A pairing that is not running.
    pub(crate) fn idle() -> Self {
        Self {
            shown: PairingState::Idle,
            deadline: None,
            held: None,
            dialing: false,
            candidates: BTreeMap::new(),
        }
    }

    /// True while the engine is looking for a device or listing candidates.
    ///
    /// An inbound connection is offered the pairing handshake only in these
    /// two states, and only while nothing is held and nothing is being
    /// dialed.
    pub(crate) fn is_open_to_pairing(&self) -> bool {
        self.held.is_none()
            && !self.dialing
            && matches!(
                self.shown,
                PairingState::Waiting | PairingState::Found { .. }
            )
    }

    /// True while pairing is running and has not reached an end state.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self.shown,
            PairingState::Waiting | PairingState::Found { .. } | PairingState::Code { .. }
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
}

impl TransferRow {
    /// The view of this row that crosses the boundary.
    pub(crate) fn info(&self) -> crate::TransferInfo {
        crate::TransferInfo {
            id: self.id.clone(),
            device_key_hex: self.device_key_hex.clone(),
            file_name: self.file_name.clone(),
            bytes_total: self.bytes_total,
            bytes_done: self.bytes_done,
            state: self.state,
            transport: self.transport,
            error: self.error.clone(),
        }
    }
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
}

impl State {
    /// A fresh state around a loaded peer store.
    pub(crate) fn new(peers: PeerStore) -> Self {
        Self {
            started: false,
            stopped: false,
            reachable: false,
            listen_addr: None,
            peers,
            live: BTreeMap::new(),
            pairing: Pairing::idle(),
            transfers: BTreeMap::new(),
            queue: VecDeque::new(),
            workers: 0,
            forwards: Vec::new(),
            discovered: Vec::new(),
        }
    }

    /// The live record for one device, created if it is not there yet.
    pub(crate) fn live_mut(&mut self, key_hex: &str) -> &mut DeviceLive {
        self.live.entry(key_hex.to_owned()).or_default()
    }

    /// The device list, as the app shows it.
    pub(crate) fn devices(&self) -> Vec<DeviceInfo> {
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
