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
use ferry_core::noise::PublicKey;
use ferry_core::path::RemotePath;
use ferry_core::peers::PeerStore;
use ferry_core::tcp::PairedConnection;

use crate::{
    DeviceInfo, Direction, FerryError, PairingCandidate, PairingState, TransferState, Transport,
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
    /// The same deadline, in Unix seconds, for the wire. Set and cleared
    /// together with `deadline`.
    pub(crate) deadline_unix_secs: Option<i64>,
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
            deadline_unix_secs: None,
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
                PairingState::Waiting { .. } | PairingState::Found { .. }
            )
    }

    /// True while pairing is running and has not reached an end state.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self.shown,
            PairingState::Waiting { .. } | PairingState::Found { .. } | PairingState::Code { .. }
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
}

/// The chunk count a size implies, and how many of those chunks are
/// verified, given how many bytes are verified in place.
///
/// Nothing new is stored for this. Both numbers come from `bytes_total`,
/// `bytes_done`, and the chunk size this run uses.
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
    ///
    /// `chunk_size` is this run's chunk size, needed to derive the chunk
    /// counts. See [`chunk_counts`].
    pub(crate) fn info(&self, chunk_size: ChunkSize) -> crate::TransferInfo {
        let (chunks_total, chunks_verified) =
            chunk_counts(self.bytes_total, self.bytes_done, chunk_size);
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
    use super::{DeviceLive, Transport, WIFI_SUCCESS_LIFETIME_SECS, available_transports};

    #[test]
    fn no_live_record_means_no_transport() {
        assert_eq!(available_transports(None, 1_000), Vec::new());
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
