//! `new`, `start`, `stop`, and what the engine reports about its own
//! run: `status`, `set_reachable`, `short_code`, the roots and download
//! folder it serves from, and the hidden hooks the integration tests call.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ferry_core::adb::{Adb, find_adb};
use ferry_core::chunk::ChunkSize;
use ferry_core::localfs::LocalFs;
use ferry_core::noise::StaticKey;
use ferry_core::peers::{DeviceKind as CoreDeviceKind, PeerStore};
use ferry_core::roots::{RootSpec, Roots};
use ferry_core::rpc::MAX_NAME_LEN;
use ferry_core::tcp::Listener;
use zeroize::Zeroize;

use crate::access::{AccessLog, RollUp};
use crate::dav;
use crate::engine::{
    DirLock, Engine, PAIRING_TIMEOUT, Shared, accept_loop, access_log_loop, adb_loop,
    add_candidate, browse_loop, last_four, load_saved_batches, load_saved_transfers, notify,
    open_roots, remember_address, wake_the_listener, wifi_candidate,
};
use crate::errors::{bad_config, failed, from_chunk_size, from_noise, from_roots};
use crate::guard::RootsState;
use crate::networks::apply_presence;
use crate::notify::{Change, Notify};
use crate::state::{State, lock, now_unix_secs};
use crate::transfer::{self, BACKOFF_MIN};
use crate::{Config, EngineListener, FerryError, Root, Status};

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
            list_cache_ttl: Arc::new(Mutex::new(crate::dav::cache::TTL)),
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
            last_probe: Mutex::new(HashMap::new()),
            probes: AtomicU64::new(0),
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

    /// How many reachability probes this engine has started.
    ///
    /// `docs/engine-contract.md`, item 3. The item 3 test needs it to prove
    /// that two discovery events inside `PROBE_MIN_INTERVAL_SECS` start one
    /// probe, not two. A probe leaves no transfer and no access log entry
    /// of its own, and it reuses a pooled connection when there is one, so
    /// the peer's accepted connection count cannot tell a second probe from
    /// no second probe. It counts up and never down, and is raised before
    /// the probe's thread is spawned, so the test reads it straight after
    /// the call that makes the discovery event. It is not exported to the
    /// apps.
    #[doc(hidden)]
    #[must_use]
    pub fn probes(&self) -> u64 {
        self.shared.probes.load(Ordering::SeqCst)
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

    /// Use a different lifetime for the Finder bridge's listing cache. For
    /// tests only.
    ///
    /// A test that asserts a second listing hits the cache cannot depend on
    /// the two listings landing within two seconds; a debug build on a slow
    /// machine spends longer than that on the first listing's prefetch.
    #[doc(hidden)]
    pub fn set_list_cache_ttl(&self, ttl: Duration) {
        *lock(&self.shared.list_cache_ttl) = ttl;
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
