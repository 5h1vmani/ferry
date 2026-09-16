//! The engine object and the threads it owns.

// The module map. This file keeps the `Engine` type, the constants, the
// `From` conversions at the boundary, and the `mod` and `pub(crate) use`
// lines below, so every path another module used before the split still
// resolves. The work lives beside it:
//
// - `shared.rs`: the state every thread shares, and the data folder lock.
// - `api/`: the methods the apps call, one file per concern. `UniFFI`
//   hashes each one's module path into the bindings, so moving one changes
//   the generated Swift and Kotlin, not the shape either one sees.
// - `remote.rs`: the one call every remote file operation goes through.
// - `records_load.rs`: transfer and batch records, read and written.
// - `inbound.rs`: accepting a connection and deciding who is calling.
// - `pairing.rs`: both pairing methods, end to end.
// - `serving.rs`: serving one paired peer on an open connection.
// - `discovery_loop.rs`: browsing mDNS and building candidates.
// - `probe.rs`: the short dial a discovered address starts.
// - `loops.rs`: the background loops a started engine runs.
// - `../networks.rs`: `apply_presence`, beside the rule it applies.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ferry_core::noise::StaticKey;
// Aliased: `crate::DeviceKind` is the boundary enum `Config` and `DeviceInfo`
// carry; this is `ferry-core`'s own, which `hello` and `PeerStore` speak.
use ferry_core::peers::DeviceKind as CoreDeviceKind;
use ferry_core::roots::RootSpec;

use crate::access;
use crate::errors::from_noise;
use crate::{AccessVerb, Actor, DeviceKind, FerryError, KeyPair, Root};

mod shared;

pub(crate) use shared::{DirLock, Shared, notify, record_this, save_networks, save_peers};

mod api;

mod remote;

pub(crate) use remote::remote_call;

mod records_load;

pub(crate) use records_load::{
    access_entry_from_core, entry_from_core, leaf_of, load_saved_batches, load_saved_transfers,
    open_roots, remove_batch, remove_record, rows_for_folder, total_listed_bytes,
};

mod inbound;

pub(crate) use inbound::{accept_loop, nobody, transport_for_inbound};

mod pairing;

pub(crate) use pairing::{
    Confirming, accept_pairing, accept_qr_offer, begin_pairing_deadline, candidate_peers,
    dial_for_pairing, dial_offer, pair_after_confirm, start_offering,
};

mod serving;

pub(crate) use serving::{
    SocketRegistration, register_serving, serve_connection, serve_named_stream,
};

mod discovery_loop;

pub(crate) use discovery_loop::{
    add_candidate, browse_loop, last_four, remember_address, wifi_candidate,
};

mod probe;

mod loops;

pub(crate) use loops::{
    access_log_loop, adb_loop, dial_targets, mark_reachable, wake_the_listener,
};

/// The port a phone listens on, so the Mac can name it in an `adb forward`.
///
/// The Mac has to write `adb forward tcp:0 tcp:<port>` before any connection
/// exists, and there is no way to ask the phone over the cable which port it
/// chose. So the port is fixed here and the Android app passes it as
/// `Config::listen_port`. The value sits in the range 49152 to 65535, which
/// IANA never assigns to a service, so nothing else can claim it.
pub const FERRY_PHONE_PORT: u16 = 52_931;

/// The subfolder [`api::transfers::landing_folder`] creates on a Mac peer's
/// fixed landing folder. `docs/engine-contract.md` item 5, "Where a push
/// lands": `Downloads/Ferry`, or `<first root>/Ferry` when no root is named
/// `Downloads`.
pub const LANDING_SUBFOLDER_MAC: &str = "Ferry";

/// The subfolder [`api::transfers::landing_folder`] creates on a phone
/// peer's fixed landing folder: `<first root>/Download`.
pub const LANDING_SUBFOLDER_PHONE: &str = "Download";

/// How long pairing runs before it gives up, unless a test shortens it.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the engine asks `adb` which devices are plugged in.
const ADB_POLL: Duration = Duration::from_secs(3);

/// How often the access log is pruned of day files past its retention
/// window, after the pass `start` already ran.
const ACCESS_LOG_PRUNE: Duration = Duration::from_secs(3600);

/// How long the discovery loop waits for one mDNS event before looking at
/// the stop flag again.
const BROWSE_TICK: Duration = Duration::from_millis(400);

/// How many addresses discovery keeps to try later.
const MAX_DISCOVERED: usize = 16;

/// How long one paired device is left alone after a reachability probe
/// starts, in seconds, before another discovery event may probe it again.
///
/// `docs/engine-contract.md`, item 3. An advert that flaps can produce many
/// discovery events in a row, and without this each one would start its own
/// dial. See `engine/probe.rs`.
const PROBE_MIN_INTERVAL_SECS: u64 = 5;

/// How many candidates the pairing screen holds at once.
///
/// A person picks a device from a short list. A network with hundreds of
/// answers, or one peer answering hundreds of times, must not turn that list
/// into a value the app has to carry across the boundary again and again.
const MAX_CANDIDATES: usize = 32;

/// How long the name exchange after a confirm may take.
///
/// The other side may confirm slowly, or never. This bounds the wait, which
/// matters because `stop` joins the thread that does the exchange.
const FINISH_PAIRING_DEADLINE: Duration = Duration::from_secs(10);

/// How often the wait for the name exchange looks at the stop flag.
const FINISH_PAIRING_TICK: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Conversions between the boundary's records and `ferry-core`'s own.
// ---------------------------------------------------------------------------

impl From<Root> for RootSpec {
    fn from(root: Root) -> Self {
        Self {
            name: root.name,
            path: PathBuf::from(root.path),
            writable: root.writable,
        }
    }
}

impl From<DeviceKind> for CoreDeviceKind {
    fn from(kind: DeviceKind) -> Self {
        match kind {
            DeviceKind::Phone => Self::Phone,
            DeviceKind::Mac => Self::Mac,
        }
    }
}

impl From<CoreDeviceKind> for DeviceKind {
    fn from(kind: CoreDeviceKind) -> Self {
        match kind {
            CoreDeviceKind::Phone => Self::Phone,
            CoreDeviceKind::Mac => Self::Mac,
        }
    }
}

impl From<access::AccessVerb> for AccessVerb {
    fn from(verb: access::AccessVerb) -> Self {
        match verb {
            access::AccessVerb::List => Self::List,
            access::AccessVerb::Stat => Self::Stat,
            access::AccessVerb::Read => Self::Read,
            access::AccessVerb::Write => Self::Write,
            access::AccessVerb::Truncate => Self::Truncate,
            access::AccessVerb::Rename => Self::Rename,
            access::AccessVerb::Mkdir => Self::Mkdir,
            access::AccessVerb::Delete => Self::Delete,
        }
    }
}

impl From<access::Actor> for Actor {
    fn from(actor: access::Actor) -> Self {
        match actor {
            access::Actor::Peer => Self::Peer,
            access::Actor::This => Self::This,
        }
    }
}

/// The engine both apps link. See the crate documentation for the contract.
#[derive(uniffi::Object)]
pub struct Engine {
    shared: Arc<Shared>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The port the phone listens on, so the Mac can reach it through an
/// `adb` forward before any connection exists.
///
/// The phone app passes this as `Config::listen_port`. It crosses the
/// boundary as a call rather than a constant, because `UniFFI` exports no
/// constants, and a number written twice drifts.
#[uniffi::export]
#[must_use]
pub fn phone_port() -> u16 {
    FERRY_PHONE_PORT
}

/// Make a fresh key pair for first run. The app stores it.
///
/// # Errors
///
/// Returns a `NoiseError` code when the system cannot produce a key.
#[uniffi::export]
pub fn generate_key() -> Result<KeyPair, FerryError> {
    let key = StaticKey::generate().map_err(|e| from_noise(&e))?;
    Ok(KeyPair {
        private: key.private_bytes().to_vec(),
        public: key.public().as_bytes().to_vec(),
    })
}

/// True when an inbound connection from `remote` is welcome.
///
/// `docs/engine-contract.md`, item 18. A non-loopback address is refused
/// while Wi-Fi presence is off: that is what a device staying silent in a
/// café means for a peer that already knows its address. Loopback is the
/// `adb` tunnel, so the cable still works on a network this device is quiet
/// on, which is job 2.
///
/// The loopback term needs `reachable`, not presence. `reachable` is the
/// person's own switch, and off means off: `set_reachable(false)` refuses
/// every connection, over the cable as well. What loopback survives is the
/// network half of the rule, which is the half a person never set.
///
/// `pairing_accepts_inbound` is `Pairing::accepts_inbound`, which was half
/// of this check before item 18 and still is. Presence needs `reachable`,
/// and a Mac running the QR method never turns `reachable` on, so dropping
/// this term would refuse the phone that scans the Mac's code.
///
/// Pure: every input is an argument, so a test can run each branch without
/// opening a socket.
#[doc(hidden)]
#[must_use]
pub fn welcomes_inbound(
    reachable: bool,
    wifi_presence: bool,
    pairing_accepts_inbound: bool,
    remote: SocketAddr,
) -> bool {
    wifi_presence || pairing_accepts_inbound || (reachable && remote.ip().is_loopback())
}
