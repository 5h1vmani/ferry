//! The engine object and the threads it owns.

// The module map. This file keeps the `Engine` type, the constants, the
// `From` conversions at the boundary, and the `mod` and `pub(crate) use`
// lines below, so every path another module used before the split still
// resolves. The work lives beside it:
//
// - `shared.rs`: the state every thread shares, and the data folder lock.
// - `api/`: nothing yet. The methods the apps call stay in this file,
//   because `UniFFI` hashes each one's module path into the bindings, so
//   moving one changes the generated Swift and Kotlin.
// - `remote.rs`: the one call every remote file operation goes through.
// - `records_load.rs`: transfer and batch records, read and written.
// - `inbound.rs`: accepting a connection and deciding who is calling.
// - `pairing.rs`: both pairing methods, end to end.
// - `serving.rs`: serving one paired peer on an open connection.
// - `discovery_loop.rs`: browsing mDNS and building candidates.
// - `loops.rs`: the background loops a started engine runs.
// - `../networks.rs`: `apply_presence`, beside the rule it applies.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ferry_core::adb::{Adb, find_adb};
use ferry_core::chunk::ChunkSize;
use ferry_core::limits::{MAX_READ_LEN, MAX_WRITE_LEN};
use ferry_core::localfs::LocalFs;
use ferry_core::noise::StaticKey;
use ferry_core::offer::{Offer, PairingError as OfferError};
use ferry_core::ops::OpError;
use ferry_core::path::{PathError, RemotePath};
// Aliased: `crate::DeviceKind` is the boundary enum `Config` and `DeviceInfo`
// carry; this is `ferry-core`'s own, which `hello` and `PeerStore` speak.
use ferry_core::peers::{DeviceKind as CoreDeviceKind, PeerStore};
use ferry_core::roots::{RootSpec, Roots};
use ferry_core::rpc::{Client, MAX_NAME_LEN, exchange_hello};
use ferry_core::session::SessionId;
use ferry_core::tcp::Listener;
use zeroize::Zeroize;

use crate::access::{self, AccessLog, RollUp};
use crate::batch::{self, BatchRecord};
use crate::dav;
use crate::errors::{
    bad_config, failed, failed_with, from_chunk_size, from_noise, from_offer, from_op, from_path,
    from_roots, from_rpc,
};
use crate::folder::{self, ListRecursiveError, RemoteLister};
use crate::guard::{RootsState, StopAware};
use crate::networks::apply_presence;
use crate::notify::{Change, Notify};
use crate::push;
use crate::state::{BatchRow, State, TransferRow, key_from_hex, lock, now_unix_secs};
use crate::transfer::{self, BACKOFF_MIN};
use crate::{
    AccessEntry, AccessVerb, Actor, AutoCopy, BatchInfo, Config, DeviceInfo, DeviceKind, Direction,
    EngineListener, Entry, FerryError, KeyPair, MountEndpoint, Origin, PairingMethod, PairingState,
    Root, Status, TransferInfo, TransferState,
};

mod shared;

pub(crate) use shared::{DirLock, Shared, notify, record_this, save_networks, save_peers};

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

/// How long pairing runs before it gives up, unless a test shortens it.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the engine asks `adb` which devices are plugged in.
const ADB_POLL: Duration = Duration::from_secs(3);

/// How often the access log roll-up is ticked, so an idle entry is
/// finalised within this long of going quiet, and the listener is told
/// within this long of that. Matches `notify.rs`'s own `HOLD`.
const ACCESS_LOG_TICK: Duration = Duration::from_millis(250);

/// How often the access log is pruned of day files past its retention
/// window, after the pass `start` already ran.
const ACCESS_LOG_PRUNE: Duration = Duration::from_secs(3600);

/// How long the discovery loop waits for one mDNS event before looking at
/// the stop flag again.
const BROWSE_TICK: Duration = Duration::from_millis(400);

/// How many addresses discovery keeps to try later.
const MAX_DISCOVERED: usize = 16;

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

// Every string crosses the foreign boundary as an owned `String`. UniFFI
// fixes these signatures, not this code, so passing by value is not a
// choice that could be made differently here.
#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
    /// Build an engine. This opens storage. It starts no thread.
    ///
    /// The data directory is created if it is missing, the paired device
    /// list is read, and any transfer records left by an earlier run are
    /// loaded as paused. [`Engine::start`] then opens the network.
    ///
    /// One engine at a time may use a data directory. The second one is
    /// refused, because two engines each hold the whole paired device list
    /// in memory and each write the whole file, so the second to write puts
    /// back what the first removed.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::BadConfig` when a directory cannot be made, the
    /// device list cannot be read, `shared_roots` is empty, or another
    /// engine is already using the directory; `Runtime::NameTooLong` when
    /// the display name is over 64 bytes; and a `NoiseError` code when the
    /// key is not two lots of 32 bytes.
    ///
    /// Opening `shared_roots` on disk is [`Engine::start`]'s job, not this
    /// one's, the same as the old single shared root: a phone builds its
    /// engine before storage permission is granted, and only `start` has to
    /// wait for it. So a root that is wrong in some way `Config` validation
    /// cannot see, such as two roots that overlap, is not caught here; it
    /// surfaces as a `RootsError` code from `start`.
    #[uniffi::constructor]
    pub fn new(config: Config, listener: Box<dyn EngineListener>) -> Result<Arc<Self>, FerryError> {
        let mut config = config;
        let key = StaticKey::from_stored(&config.key.private, &config.key.public)
            .map_err(|e| from_noise(&e));
        // The private bytes are wiped whether or not they were usable, and
        // before the error leaves this function.
        config.key.private.zeroize();
        let key = key?;

        if config.display_name.is_empty() || config.display_name.len() > MAX_NAME_LEN {
            return Err(failed("Runtime::NameTooLong"));
        }

        // This much of `shared_roots` can be validated without touching
        // disk, so `Config` validation catches it here. `start` opens the
        // roots themselves.
        if config.shared_roots.is_empty() {
            return Err(bad_config("There must be at least one shared root."));
        }

        let data_dir = PathBuf::from(&config.data_dir);
        std::fs::create_dir_all(&data_dir)
            .map_err(|_| bad_config("The engine's own folder could not be made."))?;
        let transfers_dir = data_dir.join("transfers");
        std::fs::create_dir_all(&transfers_dir)
            .map_err(|_| bad_config("The transfers folder could not be made."))?;
        let batches_dir = data_dir.join("batches");
        std::fs::create_dir_all(&batches_dir)
            .map_err(|_| bad_config("The batches folder could not be made."))?;

        let kind: CoreDeviceKind = config.kind.into();
        // A version 1 peer file predates the kind byte, so every peer in it
        // is assumed to be the opposite of this device: with one Mac and
        // one phone, that is always right.
        let assumed_peer_kind = match kind {
            CoreDeviceKind::Mac => CoreDeviceKind::Phone,
            CoreDeviceKind::Phone => CoreDeviceKind::Mac,
        };
        let peers = PeerStore::load(&data_dir.join("peers.bin"), assumed_peer_kind)
            .map_err(|_| bad_config("The paired device list could not be read."))?;
        // docs/engine-contract.md item 14: a corrupt or missing file here is
        // not a startup failure. See `auto_copy.rs` and `held.rs`.
        let auto_copy = crate::auto_copy::AutoCopyStore::load(&data_dir.join("auto_copy"));
        let held = crate::held::HeldStore::load(&data_dir.join("held"));
        // docs/engine-contract.md item 18: a missing or unreadable file here
        // is an empty list, which trusts every network. See `networks.rs`.
        let trusted = crate::networks::TrustedNetworks::load(&data_dir);

        // Last, because nothing below it can fail and leave the claim behind.
        let dir_lock = DirLock::take(&data_dir.join("lock"))?;

        let shared = Arc::new(Shared {
            notify: Notify::new(listener),
            key,
            display_name: config.display_name.clone(),
            kind,
            data_dir,
            transfers_dir,
            batches_dir,
            listen_port: config.listen_port,
            state: Mutex::new(State::new(peers, trusted)),
            wake: Condvar::new(),
            stopping: Arc::new(AtomicBool::new(false)),
            roots: Arc::new(Mutex::new(None)),
            initial_roots: config.shared_roots.clone(),
            download_dir_config: PathBuf::from(&config.download_dir),
            download_fs: Mutex::new(None),
            net: Mutex::new(None),
            advertiser: Mutex::new(None),
            adb: find_adb().map(Adb::new),
            joins: Mutex::new(Vec::new()),
            pairing_timeout: Mutex::new(PAIRING_TIMEOUT),
            backoff_min: Mutex::new(BACKOFF_MIN),
            chunk_size: Mutex::new(ChunkSize::one_mebibyte()),
            cut: Mutex::new(None),
            wire_bytes: Arc::new(AtomicU64::new(0)),
            peers_write: Mutex::new(()),
            networks_write: Mutex::new(()),
            presence: Mutex::new(()),
            dir_lock,
            access_log: Arc::new(Mutex::new(None)),
            next_connection: AtomicU64::new(0),
            mounts: dav::MountRegistry::new(),
            accepted: AtomicU64::new(0),
            inbound: AtomicU32::new(0),
            pools: Mutex::new(HashMap::new()),
            sockets: Mutex::new(HashMap::new()),
            download_dir: Mutex::new(PathBuf::from(&config.download_dir)),
            auto_copy: Mutex::new(auto_copy),
            held: Mutex::new(held),
            auto_copy_running: Mutex::new(std::collections::HashSet::new()),
            browsing: AtomicBool::new(false),
        });

        load_saved_transfers(&shared);
        load_saved_batches(&shared);
        Ok(Arc::new(Self { shared }))
    }

    /// Open the served roots and the download folder, bind the listener,
    /// and start every loop.
    ///
    /// A machine with no `adb` is not an error. USB is simply unavailable.
    ///
    /// # Errors
    ///
    /// Returns a `RootsError` code when the roots given to `new` cannot be
    /// opened, such as two that overlap or a path that is not an existing
    /// folder, unless `set_roots` already opened a fresher set; and
    /// `Runtime::BadConfig` with a detail when some other part fails to
    /// open.
    pub fn start(&self) -> Result<(), FerryError> {
        if lock(&self.shared.state).started {
            return Ok(());
        }

        // `set_roots` may have already opened a set before `start` ever
        // ran; that one wins, since it is the fresher of the two.
        if lock(&self.shared.roots).is_none() {
            let roots_state = open_roots(&self.shared.initial_roots)?;
            *lock(&self.shared.roots) = Some(roots_state);
        }

        // A pull never writes into a served root, so this opens its own
        // folder rather than reusing `self.shared.roots`. `set_download_dir`
        // may likewise have already opened a fresher one before `start`
        // ever ran; that one wins, the same way a pre-`start` `set_roots`
        // call does, just above.
        let download_fs = if lock(&self.shared.download_fs).is_none() {
            std::fs::create_dir_all(&self.shared.download_dir_config)
                .map_err(|_| bad_config("The download folder could not be made."))?;
            Some(
                LocalFs::open(&self.shared.download_dir_config)
                    .map_err(|_| bad_config("The download folder could not be opened."))?,
            )
        } else {
            None
        };

        // docs/engine-contract.md, item 13: opened at start, pruned of
        // anything past its retention window right away, and dropped at
        // stop.
        let access_log = AccessLog::open(&self.shared.data_dir)
            .map_err(|_| bad_config("The access log folder could not be opened."))?;
        let mut access_log = RollUp::new(access_log);
        drop(access_log.prune(now_unix_secs()));
        *lock(&self.shared.access_log) = Some(access_log);
        let addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            self.shared.listen_port,
        ));
        let net = Listener::bind(addr)
            .map_err(|_| bad_config("The network port could not be opened."))?;
        let local_addr = net.local_addr();

        if let Some(download_fs) = download_fs {
            *lock(&self.shared.download_fs) = Some(Arc::new(download_fs));
        }
        let net = Arc::new(net);
        *lock(&self.shared.net) = Some(Arc::clone(&net));
        {
            let mut state = lock(&self.shared.state);
            state.started = true;
            state.listen_addr = Some(local_addr);
        }

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || accept_loop(&shared, &net)));

        // Discovery runs for the whole session, not only while pairing. A
        // paired phone changes address every time it rejoins a network, and
        // the Mac has to find it again with nobody doing anything. See job 1
        // and job 3 in docs/jobs.md.
        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || browse_loop(&shared)));

        if self.shared.adb.is_some() {
            let shared = Arc::clone(&self.shared);
            self.shared
                .keep(std::thread::spawn(move || adb_loop(&shared)));
        }

        // Unlike `adb_loop`, this runs on every build: pruning and the
        // access log's idle rule do not depend on `adb` being present.
        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || access_log_loop(&shared)));

        // docs/engine-contract.md item 18: applies whatever `set_reachable`,
        // `set_network`, `trust_network`, or `forget_network` recorded
        // before `start` ran. The listener now has a port for the
        // advertiser, and the browse loop just spawned has a thread to read
        // the flag, so this is the only thing that starts the advertiser or
        // lets the browse loop hold a `Browser`.
        apply_presence(&self.shared);

        transfer::resume_all(&self.shared);
        // docs/engine-contract.md item 14: the third of the run's three
        // triggers. In practice nothing is reachable this early, since
        // `state.live` starts empty every time `new` builds a fresh state;
        // it is here for whatever later makes that not so.
        crate::auto_copy::run_for_every_reachable_enabled(&self.shared);
        Ok(())
    }

    /// Stop everything and join every loop. Safe to call twice.
    ///
    /// After this returns the listener is never called again, no file is
    /// served to any device, and the data directory is free for another
    /// engine.
    pub fn stop(&self) {
        {
            let mut state = lock(&self.shared.state);
            if state.stopped {
                return;
            }
            state.stopped = true;
            state.reachable = false;
        }
        self.shared.stopping.store(true, Ordering::SeqCst);

        // A worker blocked in a kernel read cannot see the flag just set
        // above. Closing every registered socket directly turns that read
        // into an error at once, instead of waiting for the peer or the
        // idle timeout in `tcp.rs`. `docs/engine-contract.md` item 16c.
        //
        // F4: a thread still writing to one of these sockets when it closes
        // raises no `SIGPIPE`: Rust's standard library sets `SO_NOSIGPIPE`
        // on Apple platforms and passes `MSG_NOSIGNAL` to every write on
        // Linux and Android, so the write just fails with `EPIPE` instead.
        for socket in lock(&self.shared.sockets).values() {
            drop(socket.shutdown(Shutdown::Both));
        }

        // `apply_presence` does nothing once `stopped` is set, so `stop`
        // turns both off itself. This is the last write to either: nothing
        // an app calls after this can start the advertiser again.
        // docs/engine-contract.md item 18.
        {
            let _presence = lock(&self.shared.presence);
            *lock(&self.shared.advertiser) = None;
            self.shared.browsing.store(false, Ordering::SeqCst);
        }
        self.shared.wake.notify_all();

        // A serving thread cannot be woken, so it is taken from instead:
        // every switch goes off and the served roots go away. A connection
        // that is still open refuses everything from here on, exactly as it
        // does after `forget`.
        {
            let mut state = lock(&self.shared.state);
            for live in state.live.values_mut() {
                for switch in live.serving.drain(..) {
                    switch.store(false, Ordering::SeqCst);
                }
            }
        }
        *lock(&self.shared.roots) = None;
        *lock(&self.shared.download_fs) = None;

        // The accept loop is blocked inside `accept`. A connection to our own
        // port is the only way to bring it back, since the listener has no
        // way to be woken.
        wake_the_listener(&self.shared);

        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.shared.joins));
        for handle in handles {
            // A thread that already panicked has nothing left to report to.
            drop(handle.join());
        }

        // A serving thread is never joined (`lib.rs`, "a serving thread
        // cannot be woken"), so its own `connection_ended` call may never
        // come. Taking the roll-up and finishing whatever it still has
        // pending, rather than just dropping it, is what keeps an operation
        // served in the moment before `stop` from being lost.
        if let Some(mut rollup) = lock(&self.shared.access_log).take() {
            rollup.finalize_all(now_unix_secs());
        }

        // `stop` stops every bridge. `docs/engine-contract.md`, item 6.
        self.shared.mounts.stop_all();
        // `stop` drops every pool. `docs/engine-contract.md`, item 19. Each
        // idle connection's `Pooled` unregisters its own socket as it
        // drops; the shutdown above already ended every one of them.
        lock(&self.shared.pools).clear();
        self.remove_forwards();
        *lock(&self.shared.net) = None;
        // Every joined thread has finished, so this is the last moment a
        // callback could have been made. The app is told nothing after it.
        self.shared.notify.close();
        self.shared.dir_lock.release();
    }

    /// Advertise over mDNS and accept connections, or stop doing both.
    ///
    /// Turning this off does not close connections that are already serving.
    /// New ones are refused as soon as they are accepted.
    pub fn set_reachable(&self, on: bool) {
        lock(&self.shared.state).reachable = on;
        // docs/engine-contract.md item 18: this no longer starts the
        // advertiser itself. There is one start site and it is
        // [`apply_presence`], which also decides whether this device may be
        // present on the network it is currently on.
        apply_presence(&self.shared);
        notify(&self.shared, Change::Devices);
    }

    /// The last four characters of this device's own mDNS name, while it is
    /// reachable.
    ///
    /// The Mac computes the same four characters, with the same
    /// [`last_four`], for the `short_code` it shows next to this device in
    /// its pairing candidate list. A person with several phones in the room
    /// can compare the two and tell which one they are holding.
    ///
    /// Returns `None` before [`Engine::set_reachable`] has turned advertising
    /// on, and after it has turned it off.
    #[must_use]
    pub fn short_code(&self) -> Option<String> {
        lock(&self.shared.advertiser)
            .as_ref()
            .map(|advertiser| last_four(advertiser.instance_name()))
    }

    /// Every paired device, with what is known about it right now.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceInfo> {
        lock(&self.shared.state).devices()
    }

    /// Everything this engine currently is: whether it accepts connections,
    /// what port it listens on, and whether `adb` was found.
    #[must_use]
    pub fn status(&self) -> Status {
        let state = lock(&self.shared.state);
        Status {
            reachable: state.reachable,
            listen_port: state.listen_addr.map_or(0, |addr| addr.port()),
            adb_present: self.shared.adb.is_some(),
            network: state.network.clone(),
            // Finding 5, `docs/audits/fable-engineering.md`: this reports
            // the fact, not the rule. `apply_presence` decides whether an
            // advertiser *should* run from `networks::wifi_presence`, then
            // tries to start one and drops the error if the platform
            // refuses, on purpose (`docs/engine-contract.md`, item 18). A
            // person reading this field wants to know whether this device
            // is actually announcing itself on mDNS right now, so this
            // checks the advertiser directly instead of repeating the rule.
            wifi_presence: lock(&self.shared.advertiser).is_some(),
        }
    }

    /// The app reports the name of the Wi-Fi network it is on, or `None`
    /// when it cannot read one: Wi-Fi off, the location permission refused,
    /// or the name unknown.
    ///
    /// Called after [`Engine::start`] and on every change. Idempotent: the
    /// same name twice writes nothing and reports nothing.
    ///
    /// `docs/engine-contract.md`, item 18.
    pub fn set_network(&self, name: Option<String>) {
        {
            let mut state = lock(&self.shared.state);
            if state.network == name {
                return;
            }
            state.network = name;
        }
        apply_presence(&self.shared);
        notify(&self.shared, Change::Devices);
    }

    /// Add a Wi-Fi network name to the trusted list.
    ///
    /// A name already trusted is not an error and changes nothing.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NetworkName` for an empty name, a name over 32
    /// bytes, or a 33rd name. Returns `TransferError::Local` when local
    /// storage refuses the write.
    // A name holding a control character is refused too, with the same row.
    // That sentence is deliberately not part of the public documentation:
    // uniffi copies a public doc comment into the generated bindings and
    // into the checksum both apps check, so adding it would change files
    // this fix pass must leave alone. `networks::TrustedNetworks::add` and
    // `docs/engine-contract.md`, item 18, both carry the rule.
    pub fn trust_network(&self, name: String) -> Result<(), FerryError> {
        let changed = save_networks(&self.shared, |list| list.add(&name))?;
        if changed {
            apply_presence(&self.shared);
            notify(&self.shared, Change::Devices);
        }
        Ok(())
    }

    /// Remove a Wi-Fi network name from the trusted list.
    ///
    /// A name that is not trusted is not an error and changes nothing.
    ///
    /// # Errors
    ///
    /// Returns `TransferError::Local` when local storage refuses the write.
    pub fn forget_network(&self, name: String) -> Result<(), FerryError> {
        let changed = save_networks(&self.shared, |list| list.remove(&name))?;
        if changed {
            apply_presence(&self.shared);
            notify(&self.shared, Change::Devices);
        }
        Ok(())
    }

    /// Every trusted Wi-Fi network name, oldest first.
    #[must_use]
    pub fn trusted_networks(&self) -> Vec<String> {
        lock(&self.shared.state).trusted.names().to_vec()
    }

    /// The roots currently served, as last set by `new` or `set_roots`.
    ///
    /// Before `start` has opened them, this is `Config.shared_roots` as
    /// given to `new`, unopened and unvalidated beyond being non-empty.
    #[must_use]
    pub fn roots(&self) -> Vec<Root> {
        lock(&self.shared.roots).as_ref().map_or_else(
            || self.shared.initial_roots.clone(),
            |state| state.specs.clone(),
        )
    }

    /// Replace the served roots.
    ///
    /// Takes effect for every already-connected peer on its next operation;
    /// nobody needs to reconnect. The app is responsible for persisting
    /// `roots` and passing it back in `Config` at the next launch.
    ///
    /// # Errors
    ///
    /// Returns a `RootsError` code when `roots` is refused: no roots at all,
    /// an invalid or duplicate name, a path that is not an existing folder,
    /// or two roots that overlap.
    pub fn set_roots(&self, roots: Vec<Root>) -> Result<(), FerryError> {
        let opened =
            Roots::open(roots.iter().cloned().map(RootSpec::from).collect()).map_err(from_roots)?;
        *lock(&self.shared.roots) = Some(RootsState {
            specs: roots,
            opened: Arc::new(opened),
        });
        Ok(())
    }

    /// Change where a pulled file lands.
    ///
    /// Creates the folder if it does not exist. The app is responsible for
    /// persisting `path` and passing it back in `Config` at the next launch.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::BadConfig` when the folder cannot be made or
    /// opened.
    pub fn set_download_dir(&self, path: String) -> Result<(), FerryError> {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(&path)
            .map_err(|_| bad_config("The download folder could not be made."))?;
        let fs = LocalFs::open(&path)
            .map_err(|_| bad_config("The download folder could not be opened."))?;
        *lock(&self.shared.download_fs) = Some(Arc::new(fs));
        // docs/engine-contract.md item 14: `Engine::auto_copy` reports the
        // current download folder, not the one `new` was given.
        *lock(&self.shared.download_dir) = path;
        Ok(())
    }

    /// Starts serving one device's shared roots over `WebDAV` on a random
    /// loopback port. Idempotent: a second call for a device that already
    /// has a bridge returns that same bridge's endpoint.
    ///
    /// `docs/engine-contract.md`, item 6.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when no device has that key, and
    /// `Runtime::MountFailed` when the loopback port cannot be bound or the
    /// password cannot be generated.
    pub fn mount_start(&self, device_key_hex: String) -> Result<MountEndpoint, FerryError> {
        let key = key_from_hex(&device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        // The device's own stored name becomes the mount root's
        // `displayname` (N4, `docs/manual-checks.md` Part E): read here,
        // under the state lock, rather than trusting anything a DAV
        // request could shape.
        let device_name = lock(&self.shared.state)
            .peers
            .get(&key)
            .ok_or_else(|| failed("Runtime::NotPaired"))?
            .name
            .clone();
        self.shared
            .mounts
            .start(&self.shared, &device_key_hex, &device_name)
    }

    /// Stops serving one device's shared roots over `WebDAV`, and closes its
    /// port. Safe to call on a device with no running bridge.
    ///
    /// `docs/engine-contract.md`, item 6.
    pub fn mount_stop(&self, device_key_hex: String) {
        self.shared.mounts.stop(&device_key_hex);
    }

    /// Records where the app mounted a device's bridge, or that it
    /// unmounted it. Read back through `DeviceInfo.mount_path`.
    ///
    /// `docs/engine-contract.md`, item 6.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when no device has that key.
    pub fn set_mount_path(
        &self,
        device_key_hex: String,
        path: Option<String>,
    ) -> Result<(), FerryError> {
        let key = key_from_hex(&device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        {
            let mut state = lock(&self.shared.state);
            if state.peers.get(&key).is_none() {
                return Err(failed("Runtime::NotPaired"));
            }
            state.live_mut(&device_key_hex).mount_path = path;
        }
        notify(&self.shared, Change::Devices);
        Ok(())
    }

    /// Job 7: whether this device copies a paired device's camera folder to
    /// itself on its own, and what its last run did.
    ///
    /// `docs/engine-contract.md`, item 14. Always answers; see [`AutoCopy`].
    #[must_use]
    pub fn auto_copy(&self, device_key_hex: String) -> AutoCopy {
        crate::auto_copy::get(&self.shared, &device_key_hex)
    }

    /// Turns automatic copying on or off for one device.
    ///
    /// `docs/engine-contract.md`, item 14.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when no device has that key.
    pub fn set_auto_copy(&self, device_key_hex: String, enabled: bool) -> Result<(), FerryError> {
        crate::auto_copy::set_enabled(&self.shared, &device_key_hex, enabled)
    }

    /// Forget a device: remove its key and every transfer record for it.
    ///
    /// A connection that is already serving this device stops answering at
    /// once, though its socket stays open until the peer goes away.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when no device has that key, a
    /// `PeerError` code when the device list cannot be written, and
    /// `TransferError::Local` when a transfer record cannot be deleted. The
    /// last one matters: a record left on disk would start the transfer
    /// again on the next run.
    pub fn forget(&self, key_hex: String) -> Result<(), FerryError> {
        let key = key_from_hex(&key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        if lock(&self.shared.state).peers.get(&key).is_none() {
            return Err(failed("Runtime::NotPaired"));
        }
        // `forget` stops the device's bridge. `docs/engine-contract.md`,
        // item 6, and closes its pool, item 19. The bridge is stopped first,
        // so no request can make the pool again after it is dropped.
        //
        // Removing the registry's handle is not enough. A bridge connection
        // Finder already holds keeps its own `Arc<Bridge>`, which keeps the
        // `Arc<Pool>`, so its next request would pop an idle connection
        // nobody had closed. `Pool::close` shuts every idle connection down
        // and refuses every later borrow.
        self.shared.mounts.stop(&key_hex);
        {
            // One hold of the pools lock covers the removal, the close, and
            // the peer list write. `Shared::pool_for` takes the same lock
            // across its own paired check, so no call can pass that check
            // while this is running and then make the pool again.
            let mut pools = lock(&self.shared.pools);
            if let Some(pool) = pools.remove(&key_hex) {
                pool.close();
            }
            save_peers(&self.shared, |store| {
                drop(store.remove(&key));
                Ok(())
            })?;
            drop(pools);
        }

        let gone: Vec<String>;
        let gone_batches: Vec<String>;
        {
            let mut state = lock(&self.shared.state);
            if let Some(live) = state.live.remove(&key_hex) {
                for switch in live.serving {
                    switch.store(false, Ordering::SeqCst);
                }
            }
            gone = state
                .transfers
                .values()
                .filter(|row| row.device_key_hex == key_hex)
                .map(|row| row.id.clone())
                .collect();
            for id in &gone {
                state.transfers.remove(id);
            }
            gone_batches = state
                .batches
                .values()
                .filter(|batch| batch.device_key_hex == key_hex)
                .map(|batch| batch.id.clone())
                .collect();
            for id in &gone_batches {
                state.batches.remove(id);
            }
        }
        let mut trouble = None;
        for id in &gone {
            if let Err(error) = remove_record(&self.shared, id) {
                trouble = Some(error);
            }
        }
        for id in &gone_batches {
            if let Err(error) = remove_batch(&self.shared, id) {
                trouble = Some(error);
            }
        }
        notify(&self.shared, Change::Devices);
        notify(&self.shared, Change::Transfers);
        match trouble {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Enter pairing, by `method`. Replaces the old `start_pairing`. Times
    /// out after two minutes either way.
    ///
    /// `Code`: the Mac browses and polls `adb`, and reports candidates. The
    /// phone waits for one `XX` handshake and reports the code.
    ///
    /// `Qr`: makes a nonce and an offer for this device's Wi-Fi addresses,
    /// and publishes `Offering`. While offering, one `IK` handshake whose
    /// message one carries the current nonce is accepted; it shows
    /// `Requested` and holds the connection for `confirm_pairing`. Meant for
    /// the Mac; the phone's camera screen is not built yet, so nothing
    /// today calls this with `Qr` on a phone.
    ///
    /// Calling this while a pairing is already running only reports the
    /// current state again, under either method.
    pub fn start_pairing_with(&self, method: PairingMethod) {
        // The running check and the claim below happen under one lock, in
        // `begin_pairing_deadline`: a second call cannot land in the gap
        // between them the way two separate lock acquisitions would allow.
        let Some(expires_unix_secs) = begin_pairing_deadline(&self.shared) else {
            let shown = lock(&self.shared.state).pairing.shown.clone();
            self.shared.notify.pairing(&shown);
            return;
        };
        match method {
            PairingMethod::Code => {
                self.shared
                    .set_pairing(&PairingState::Waiting { expires_unix_secs });
            }
            PairingMethod::Qr => start_offering(&self.shared, expires_unix_secs),
        }
    }

    /// Phone only. The bytes its camera decoded from the Mac's QR code.
    ///
    /// Checked locally, in order: is this a Ferry offer at all, has it
    /// expired, is its key one this device already holds. Any of those
    /// three refuses at once, before a single byte reaches the network.
    /// Past that point the dial and the `IK` handshake run on their own
    /// thread, as `pick_candidate` runs its dial, and the outcome arrives
    /// through the listener. Once the names cross, this device publishes
    /// `Requested` with the other device's name and waits for
    /// `confirm_pairing`, the same as the offering Mac does. The scan proves
    /// the key came from a screen; it does not show whose screen, so this
    /// side asks that question before it stores anything.
    ///
    /// # Errors
    ///
    /// Returns `PairingError::OfferNotFerry`, `PairingError::OfferExpired`,
    /// or `PairingError::AlreadyPaired` for the three local checks above,
    /// and `Runtime::PairingBusy` when a pairing attempt is already running
    /// on this device.
    pub fn offer_scanned(&self, payload: Vec<u8>) -> Result<(), FerryError> {
        let offer = Offer::decode(&payload).map_err(from_offer)?;
        if offer.is_expired(now_unix_secs()) {
            return Err(from_offer(OfferError::OfferExpired));
        }
        if lock(&self.shared.state)
            .peers
            .get(&offer.static_key)
            .is_some()
        {
            return Err(from_offer(OfferError::AlreadyPaired));
        }
        // The running check and the claim happen under one lock; see
        // `begin_pairing_deadline`.
        let Some(expires_unix_secs) = begin_pairing_deadline(&self.shared) else {
            return Err(failed("Runtime::PairingBusy"));
        };
        self.shared
            .set_pairing(&PairingState::Waiting { expires_unix_secs });

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || dial_offer(&shared, &offer)));
        Ok(())
    }

    /// Dial the chosen candidate and run the pairing handshake.
    ///
    /// The dial happens on its own thread, so this returns at once. The code
    /// arrives through the listener.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NoCandidate` when that candidate is not listed, and
    /// `Runtime::PairingBusy` when a code is already showing or another
    /// candidate is already being dialed.
    pub fn pick_candidate(&self, id: String) -> Result<(), FerryError> {
        let addr = {
            let mut state = lock(&self.shared.state);
            if !state.pairing.is_running() {
                return Err(failed("Runtime::NoCandidate"));
            }
            if !state.pairing.is_open_to_pairing() {
                return Err(failed("Runtime::PairingBusy"));
            }
            let addr = state
                .pairing
                .candidates
                .get(&id)
                .map(|c| c.addr)
                .ok_or_else(|| failed("Runtime::NoCandidate"))?;
            // One dial at a time, from this moment and not from the moment
            // the handshake finishes. Two dials would show two codes.
            state.pairing.dialing = true;
            addr
        };

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || dial_for_pairing(&shared, addr)));
        Ok(())
    }

    /// Accept or reject the device whose code, or scan, is showing.
    ///
    /// Accepting stores the peer, and on most paths exchanges names first.
    /// That takes a round trip, so it runs on its own thread and reports
    /// through the listener. Works the same way for both pairing methods
    /// and for both sides of a scan: whichever of `held` (code) or
    /// `requested` (QR) is holding a connection is the one taken.
    ///
    /// This answers for this device only. The other device answers its own
    /// question on its own screen, and neither answer stores anything on
    /// the other. `docs/engine-contract.md` item 12.
    pub fn confirm_pairing(&self, accept: bool) {
        let taken = {
            let mut state = lock(&self.shared.state);
            state
                .pairing
                .held
                .take()
                .map(Confirming::Code)
                .or_else(|| state.pairing.requested.take().map(Confirming::Scan))
        };
        let Some(taken) = taken else {
            if !accept {
                self.shared.set_pairing(&PairingState::Idle);
            }
            return;
        };
        if !accept {
            drop(taken);
            self.shared.set_pairing(&PairingState::Idle);
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.shared.keep(std::thread::spawn(move || {
            pair_after_confirm(&shared, taken);
        }));
    }

    /// Stop pairing and drop whatever it was holding, under either method.
    pub fn cancel_pairing(&self) {
        {
            let mut state = lock(&self.shared.state);
            state.pairing.held = None;
            state.pairing.dialing = false;
            state.pairing.requested = None;
            state.pairing.offer_nonce = None;
        }
        self.shared.set_pairing(&PairingState::Idle);
    }

    /// Every transfer, as the app shows them.
    #[must_use]
    pub fn transfers(&self) -> Vec<TransferInfo> {
        lock(&self.shared.state)
            .transfers
            .values()
            .map(TransferRow::info)
            .collect()
    }

    /// Every batch this engine has grouped, across every device.
    #[must_use]
    pub fn batches(&self) -> Vec<BatchInfo> {
        let state = lock(&self.shared.state);
        state
            .batches
            .values()
            .map(|batch| batch.info(&state.transfers))
            .collect()
    }

    /// The access log, newest first. `None` for `device_key_hex` returns
    /// every device's. `limit` is capped at 1,000. Empty before `start` has
    /// opened the store.
    ///
    /// `docs/engine-contract.md`, batch E, item 13.
    #[must_use]
    pub fn access_log(&self, device_key_hex: Option<String>, limit: u32) -> Vec<AccessEntry> {
        lock(&self.shared.access_log)
            .as_ref()
            .map(|rollup| rollup.store().query(device_key_hex.as_deref(), limit))
            .unwrap_or_default()
            .into_iter()
            .map(access_entry_from_core)
            .collect()
    }

    /// Fetch one file from a paired device into the shared root.
    ///
    /// Returns the transfer identifier. The work runs on its own thread and
    /// reports through the listener.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when either path is refused,
    /// `Runtime::NotPaired` when that device is not stored, and
    /// `Runtime::NotStarted` before [`Engine::start`] has run.
    /// `PathError::Empty` is one such code: it names the shared root, which
    /// has no single file to pull.
    pub fn pull(
        &self,
        device_key_hex: String,
        remote_path: String,
        local_name: String,
    ) -> Result<String, FerryError> {
        let source = RemotePath::parse(&remote_path).map_err(from_path)?;
        if source.is_root() {
            return Err(from_path(PathError::Empty));
        }
        let destination = RemotePath::parse(&local_name).map_err(from_path)?;
        // Refused here and not only where the transfer is built, because
        // the first pass fetches the whole file before that point.
        if destination.is_root() {
            return Err(from_path(PathError::Empty));
        }
        let key = key_from_hex(&device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;

        let id = {
            let mut state = lock(&self.shared.state);
            if !state.started {
                return Err(failed("Runtime::NotStarted"));
            }
            if state.peers.get(&key).is_none() {
                return Err(failed("Runtime::NotPaired"));
            }
            let session =
                SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
            // The key is part of the identifier so that a restart can tell
            // which device a record on disk belongs to, and so `forget` can
            // find every record for one device by its name alone.
            let id = format!("{device_key_hex}-{session}");
            let file_name = leaf_of(&destination);
            state.transfers.insert(
                id.clone(),
                TransferRow {
                    id: id.clone(),
                    device_key_hex: device_key_hex.clone(),
                    file_name,
                    source,
                    destination,
                    bytes_total: 0,
                    bytes_done: 0,
                    state: TransferState::Queued,
                    transport: None,
                    error: None,
                    source_size: None,
                    source_mtime: None,
                    running: false,
                    attempt_after: None,
                    backoff: BACKOFF_MIN,
                    started_unix_secs: now_unix_secs(),
                    ended_unix_secs: None,
                    direction: Direction::Pull,
                    speed_bytes_per_sec: None,
                    batch_id: None,
                    chunk_size: *lock(&self.shared.chunk_size),
                },
            );
            id
        };

        notify(&self.shared, Change::Transfers);
        transfer::spawn(&self.shared, &id);
        Ok(id)
    }

    /// Copy a whole folder into one batch.
    ///
    /// Lists `remote_path` recursively over the connection, using the same
    /// paging `list` uses, then creates the batch and queues one transfer
    /// per file found, in listing order. Blocks until the listing is done,
    /// so the app calls it off the main thread, the same way it calls
    /// `list`.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when the path is refused,
    /// `Runtime::NotPaired` when the device is not stored,
    /// `Runtime::NotStarted` before [`Engine::start`] has run,
    /// `Runtime::NotReachable` when no dial succeeds, an `OpError` code when
    /// the peer refuses the folder itself, and `Runtime::FolderTooLarge` at
    /// more than 10,000 files or more than 32 levels of nesting. Nothing is
    /// queued when this returns an error.
    pub fn pull_folder(
        &self,
        device_key_hex: String,
        remote_path: String,
    ) -> Result<String, FerryError> {
        let source = RemotePath::parse(&remote_path).map_err(from_path)?;
        if source.is_root() {
            return Err(from_path(PathError::Empty));
        }
        let key = key_from_hex(&device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        {
            let state = lock(&self.shared.state);
            if !state.started {
                return Err(failed("Runtime::NotStarted"));
            }
            if state.peers.get(&key).is_none() {
                return Err(failed("Runtime::NotPaired"));
            }
        }

        let (stream, socket, addr, via) = transfer::dial(&self.shared, &device_key_hex, &key)?;
        mark_reachable(&self.shared, &device_key_hex, addr, via);
        // docs/engine-contract.md item 16c: registered for the life of this
        // call, so `stop` can close it if the listing hangs.
        let connection_id = self.shared.next_connection_id();
        let _socket = SocketRegistration::new(&self.shared, connection_id, socket);
        let mut stream = StopAware::new(stream, Arc::clone(&self.shared.stopping));
        exchange_hello(&mut stream, &self.shared.display_name, self.shared.kind)
            .map_err(|error| from_rpc(&error))?;
        let mut client = Client::new(stream);

        let lister = RemoteLister::new(&mut client);
        let found_files =
            folder::list_recursive(&lister, &source).map_err(|error| match error {
                ListRecursiveError::TooLarge => failed("Runtime::FolderTooLarge"),
                ListRecursiveError::Op(OpError::Internal) => lister
                    .take_failure()
                    .map_or_else(|| from_op(OpError::Internal), |rpc| from_rpc(&rpc)),
                ListRecursiveError::Op(op) => from_op(op),
            })?;

        let leaf = leaf_of(&source);
        let prefix = format!("{}/", source.as_str());
        let started_unix_secs = now_unix_secs();

        // docs/engine-contract.md, item 13: one Read entry for the whole
        // listing, with the file count and the byte total the listing
        // itself already reported, before a single byte of any file has
        // moved.
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Read,
            source.as_str(),
            Some(total_listed_bytes(&found_files)),
            None,
            Some(u32::try_from(found_files.len()).unwrap_or(u32::MAX)),
        );

        let mut rows = rows_for_folder(
            &found_files,
            &device_key_hex,
            &leaf,
            &prefix,
            started_unix_secs,
            *lock(&self.shared.chunk_size),
        )?;

        let batch_session =
            SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        let batch_id = format!("{device_key_hex}-{batch_session}");
        for row in &mut rows {
            row.batch_id = Some(batch_id.clone());
        }
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let batch_row = BatchRow {
            id: batch_id.clone(),
            device_key_hex: device_key_hex.clone(),
            label: remote_path,
            direction: Direction::Pull,
            origin: Origin::Manual,
            started_unix_secs,
            transfer_ids: ids.clone(),
            done_files: 0,
            done_bytes: 0,
        };
        batch::write_batch(
            &self.shared.batch_path(&batch_id),
            &BatchRecord::of(&batch_row),
        )?;

        {
            let mut state = lock(&self.shared.state);
            state.batches.insert(batch_id.clone(), batch_row);
            for row in rows {
                state.transfers.insert(row.id.clone(), row);
            }
        }
        notify(&self.shared, Change::Transfers);
        for id in &ids {
            transfer::spawn(&self.shared, id);
        }
        Ok(batch_id)
    }

    /// Send one file to a paired device.
    ///
    /// `docs/engine-contract.md`, item 5. `local_path` is absolute on this
    /// device; `remote_path` is root-relative on the peer and names the
    /// file, not its folder. Runs on its own thread, the same as `pull`, and
    /// resumes on its own when the device becomes reachable again.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when `remote_path` is refused, and
    /// `Runtime::NotPaired` when the device is not stored. A local file that
    /// is missing, a directory, a symlink, or a special file, and a
    /// read-only root on the peer, surface as the matching error on the
    /// transfer row instead, once a worker attempts it. See `push.rs`.
    pub fn push(
        &self,
        device_key_hex: String,
        local_path: String,
        remote_path: String,
    ) -> Result<String, FerryError> {
        push::push(&self.shared, &device_key_hex, &local_path, &remote_path)
    }

    /// Send several files into one folder on a paired device, as one batch.
    ///
    /// `docs/engine-contract.md`, item 5. Each file lands at
    /// `remote_folder/<file name>`. Dials the device to confirm
    /// `remote_folder` is really a folder before anything is queued, the
    /// same way `pull_folder` confirms its own folder by listing it.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired`, `Runtime::NotStarted`, an `OpError`
    /// code when the dial or the folder check fails, and
    /// `OpError::NotADirectory` when `remote_folder` names a file on the
    /// peer.
    pub fn push_files(
        &self,
        device_key_hex: String,
        local_paths: Vec<String>,
        remote_folder: String,
    ) -> Result<String, FerryError> {
        push::push_files(&self.shared, &device_key_hex, &local_paths, &remote_folder)
    }

    /// List every entry in one folder on a paired device.
    ///
    /// Borrows one of the device's four pooled connections, then pages
    /// through the server's cursor until it reports no more entries, and
    /// returns them in the order the server sent them. This blocks for one
    /// round trip per page, so the app must call it off the main thread.
    ///
    /// `docs/engine-contract.md`, item 19: the pool is the engine's, shared
    /// with the `WebDAV` bridge, so two listings in a row reuse one
    /// connection rather than dialling twice.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when the path is refused,
    /// `Runtime::NotPaired` when that device is not stored,
    /// `Runtime::NotStarted` before [`Engine::start`] has run,
    /// `Runtime::NotReachable` when no dial succeeds, an `OpError` code
    /// when the peer refuses, such as `OpError::NotFound` for a folder that
    /// does not exist, and `Runtime::FolderTooLarge` when the peer pages the
    /// folder past the bounds `folder::after_page` checks, such as a
    /// `next_cursor` that never advances.
    pub fn list(
        &self,
        device_key_hex: String,
        remote_path: String,
    ) -> Result<Vec<Entry>, FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;

        let mut entries = Vec::new();
        let mut cursor = 0u64;
        let mut pages = 0usize;
        let mut entries_seen = 0usize;
        loop {
            let (page, next_cursor) = borrowed.call(|client| client.list(&path, cursor))?;
            pages += 1;
            entries_seen += page.len();
            entries.extend(page.into_iter().map(entry_from_core));
            match folder::after_page(cursor, next_cursor, pages, entries_seen) {
                Ok(Some(next)) => cursor = next,
                Ok(None) => break,
                Err(folder::ListTooLarge) => return Err(failed("Runtime::FolderTooLarge")),
            }
        }
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::List,
            path.as_str(),
            None,
            Some(u32::try_from(entries.len()).unwrap_or(u32::MAX)),
            None,
        );
        Ok(entries)
    }

    /// Describe one file or folder on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. The phone's `DocumentsProvider`
    /// answers `queryDocument` with this. Blocks for one round trip, so the
    /// app calls it off the main thread, as it does [`Engine::list`].
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], including `OpError::NotFound` for a path that
    /// names no file.
    pub fn stat(&self, device_key_hex: String, remote_path: String) -> Result<Entry, FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        let entry = borrowed.call(|client| client.stat(&path))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Stat,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(entry_from_core(entry))
    }

    /// Read a byte range from a file on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. At most [`MAX_READ_LEN`] bytes,
    /// one mebibyte. A longer ask is clamped, not refused, so the caller
    /// gets a short read, which is an ordinary read result: fewer bytes
    /// than asked for also means the end of the file.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], including `OpError::IsADirectory` when the path
    /// names a folder.
    pub fn read_at(
        &self,
        device_key_hex: String,
        remote_path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let want = len.min(MAX_READ_LEN);
        let mut borrowed = pool.take_dialing(&self.shared)?;
        let bytes = borrowed.call(|client| client.read(&path, offset, want))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Read,
            path.as_str(),
            Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            None,
            None,
        );
        Ok(bytes)
    }

    /// Write a byte range to a file on a paired device, creating the file
    /// when it does not exist.
    ///
    /// `docs/engine-contract.md`, item 19. More than [`MAX_WRITE_LEN`]
    /// bytes, one mebibyte, in one call is refused before anything reaches
    /// the wire, so a refused call writes nothing.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `Runtime::WriteTooLarge` when `bytes` is
    /// longer than one mebibyte, and `OpError::PermissionDenied` when the
    /// peer's root is not writable.
    pub fn write_at(
        &self,
        device_key_hex: String,
        remote_path: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        // Before the pool is touched, so a refused write neither dials nor
        // sends a byte.
        if bytes.len() > MAX_WRITE_LEN as usize {
            return Err(failed("Runtime::WriteTooLarge"));
        }
        let mut borrowed = pool.take_dialing(&self.shared)?;
        let written = borrowed.call(|client| client.write(&path, offset, bytes))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Write,
            path.as_str(),
            Some(u64::from(written)),
            None,
            None,
        );
        Ok(())
    }

    /// Set a file's length on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. The phone's provider truncates
    /// to zero when it opens a document in a truncating mode.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::PermissionDenied` when the
    /// peer's root is not writable.
    pub fn truncate(
        &self,
        device_key_hex: String,
        remote_path: String,
        len: u64,
    ) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.truncate(&path, len))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Truncate,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Make one folder on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. Makes one level only: the parent
    /// must already exist, or the peer answers `OpError::NotFound`.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::AlreadyExists` when something is
    /// already there, and `OpError::PermissionDenied` when the peer's root
    /// is not writable.
    pub fn mkdir(&self, device_key_hex: String, remote_path: String) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.mkdir(&path))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Mkdir,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Delete one file, or one empty folder, on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. The wire has no recursive
    /// delete, so a folder with anything in it is refused with
    /// `OpError::NotEmpty`. A caller that wants the folder gone walks it
    /// and deletes the leaves first, as the `WebDAV` bridge does.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::NotEmpty` for a folder that
    /// still holds something, and `OpError::PermissionDenied` when the
    /// peer's root is not writable.
    pub fn delete(&self, device_key_hex: String, remote_path: String) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.delete(&path))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Delete,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Move or rename a file or folder on a paired device, within one root.
    ///
    /// `docs/engine-contract.md`, item 19. Across two roots the peer
    /// answers `OpError::Unsupported`, the same refusal the `WebDAV`
    /// bridge turns into 502.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::Unsupported` for a move across
    /// roots, `OpError::AlreadyExists` when something is already at `to`,
    /// and `OpError::PermissionDenied` when the peer's root is not
    /// writable.
    pub fn rename(
        &self,
        device_key_hex: String,
        from: String,
        to: String,
    ) -> Result<(), FerryError> {
        let (source, pool) = remote_call(&self.shared, &device_key_hex, &from)?;
        let destination = RemotePath::parse(&to).map_err(from_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.rename(&source, &destination))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Rename,
            // The destination, not the source, the same choice
            // `guard.rs` makes on the serving side: a person searching the
            // log looks for where a file ended up.
            destination.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Restart a failed transfer from its resume point.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::TransferNotFound` when no transfer has that
    /// identifier, and `Runtime::NotPaired` when its device has been
    /// forgotten. In the second case the transfer and its record are dropped
    /// before the error is returned.
    pub fn retry(&self, transfer_id: String) -> Result<(), FerryError> {
        {
            let mut state = lock(&self.shared.state);
            let row = state
                .transfers
                .get(&transfer_id)
                .ok_or_else(|| failed("Runtime::TransferNotFound"))?;
            // A transfer with no device behind it has nowhere to go, and a
            // record left on disk is what brings a forgotten device back.
            let paired = key_from_hex(&row.device_key_hex)
                .is_some_and(|key| state.peers.get(&key).is_some());
            if !paired {
                state.transfers.remove(&transfer_id);
                drop(state);
                drop(remove_record(&self.shared, &transfer_id));
                notify(&self.shared, Change::Transfers);
                return Err(failed("Runtime::NotPaired"));
            }
            let row = state
                .transfers
                .get_mut(&transfer_id)
                .ok_or_else(|| failed("Runtime::TransferNotFound"))?;
            // A finished transfer has no record and no partial file left, so
            // a retry would fetch the whole file again. That is a new pull,
            // not a retry.
            if row.running || row.state == TransferState::Done {
                return Ok(());
            }
            row.state = TransferState::Queued;
            row.error = None;
            row.ended_unix_secs = None;
        }
        notify(&self.shared, Change::Transfers);
        transfer::spawn(&self.shared, &transfer_id);
        Ok(())
    }

    /// Retry every `Failed` transfer in a batch.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::TransferNotFound`, with the batch id as detail,
    /// when no batch has that identifier. Otherwise behaves as calling
    /// [`Engine::retry`] on each of the batch's `Failed` transfers in turn:
    /// each one that is not paired any more is dropped rather than retried,
    /// and that device's `Runtime::NotPaired` is not itself an error here.
    pub fn retry_batch(&self, batch_id: String) -> Result<(), FerryError> {
        let ids: Vec<String> = {
            let state = lock(&self.shared.state);
            let batch = state
                .batches
                .get(&batch_id)
                .ok_or_else(|| failed_with("Runtime::TransferNotFound", &batch_id))?;
            batch
                .transfer_ids
                .iter()
                .filter(|id| {
                    state
                        .transfers
                        .get(id.as_str())
                        .is_some_and(|row| row.state == TransferState::Failed)
                })
                .cloned()
                .collect()
        };
        for id in ids {
            match self.retry(id) {
                Ok(()) => {}
                // `retry` itself already dropped the transfer and its
                // record; the whole batch retry is not a failure because
                // one device in it was forgotten mid-retry.
                Err(FerryError::Failed { code, .. }) if code == "Runtime::NotPaired" => {}
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }
}

impl Engine {
    /// Inject a Wi-Fi candidate, as if discovery had found it.
    ///
    /// The address is remembered the way a discovered address is, so a
    /// transfer can dial it later. This is the seam the integration tests
    /// use, so pairing and resuming can be proved without a working mDNS
    /// network. It is not exported to the apps.
    #[doc(hidden)]
    pub fn offer_candidate(&self, addr: SocketAddr) {
        remember_address(&self.shared, addr);
        add_candidate(&self.shared, &wifi_candidate(addr), addr);
    }

    /// How many transfer worker threads are running.
    ///
    /// The integration test needs it to prove that a hundred transfers do
    /// not become a hundred threads. It is not exported to the apps.
    #[doc(hidden)]
    #[must_use]
    pub fn transfer_workers(&self) -> u32 {
        u32::try_from(lock(&self.shared.state).workers).unwrap_or(u32::MAX)
    }

    /// The spool folder's total size right now, across every device.
    ///
    /// Kept as a running count instead of walked on every `PUT` and `COPY`
    /// (`docs/audits/fable-engineering.md`, finding 4). The integration
    /// test needs it to prove that count stays exact across two `PUT`s and
    /// a removed spool file, since a walk of the count's own bookkeeping
    /// cannot be observed any other way. It is not exported to the apps.
    #[doc(hidden)]
    #[must_use]
    pub fn spool_bytes(&self) -> u64 {
        self.shared.mounts.spool_bytes_total()
    }

    /// How many connections this engine has accepted and begun to serve.
    ///
    /// The item 19 test needs it to prove that two listings in a row reuse
    /// one pooled connection instead of dialling twice. It counts up and
    /// never down, so the test reads it before and after. It is not
    /// exported to the apps.
    #[doc(hidden)]
    #[must_use]
    pub fn accepted_connections(&self) -> u64 {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    /// The address this engine's listener is bound to, once it has started.
    ///
    /// The integration test needs it to dial the other engine. It is not
    /// exported to the apps.
    #[doc(hidden)]
    #[must_use]
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        lock(&self.shared.state).listen_addr
    }

    /// Use a different pairing timeout. For tests only.
    #[doc(hidden)]
    pub fn set_pairing_timeout(&self, timeout: Duration) {
        *lock(&self.shared.pairing_timeout) = timeout;
    }

    /// Use a different backoff floor. For tests only.
    ///
    /// A test that breaks a link on purpose does not want to wait out the
    /// ordinary one second floor before the retry it is waiting for.
    #[doc(hidden)]
    pub fn set_backoff(&self, min: Duration) {
        *lock(&self.shared.backoff_min) = min;
    }

    /// Use a different chunk size for the next first pass. For tests only.
    ///
    /// # Errors
    ///
    /// Returns a `ChunkSizeError` code when `bytes` is below 1024, above
    /// [`ChunkSize::MAX`], or not a power of two.
    #[doc(hidden)]
    pub fn set_chunk_size(&self, bytes: u32) -> Result<(), FerryError> {
        let chunk = ChunkSize::new(bytes).map_err(from_chunk_size)?;
        *lock(&self.shared.chunk_size) = chunk;
        Ok(())
    }

    /// Break the next dial after `after_bytes` bytes cross it, read and
    /// write combined. For tests only.
    ///
    /// Armed for one dial only. The dial that carries it takes it, and the
    /// dial after that has none, so a retry after the cut runs clean.
    #[doc(hidden)]
    pub fn set_cut(&self, after_bytes: u64) {
        *lock(&self.shared.cut) = Some(after_bytes);
    }

    /// Bytes moved on the wire since this engine started, across every dial.
    ///
    /// Counts every byte a transfer's stream reads or writes, whether or not
    /// a cut is armed. For tests only.
    #[doc(hidden)]
    #[must_use]
    pub fn wire_bytes(&self) -> u64 {
        self.shared.wire_bytes.load(Ordering::SeqCst)
    }

    /// Remove every `adb` forward this engine opened.
    fn remove_forwards(&self) {
        let forwards = std::mem::take(&mut lock(&self.shared.state).forwards);
        let Some(adb) = self.shared.adb.as_ref() else {
            return;
        };
        for forward in forwards {
            // A cable already unplugged has removed the forward for us.
            drop(adb.remove_forward(&forward.serial, forward.local_port));
        }
    }
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
