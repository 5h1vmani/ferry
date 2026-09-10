//! The engine object and the threads it owns.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ferry_core::adb::{Adb, find_adb};
use ferry_core::discovery::{Advertiser, Browser, Event};
use ferry_core::localfs::LocalFs;
use ferry_core::noise::{PublicKey, StaticKey};
use ferry_core::path::RemotePath;
use ferry_core::peers::{Peer, PeerStore};
use ferry_core::rpc::{MAX_NAME_LEN, exchange_hello, serve};
use ferry_core::session::{SessionId, Transfer};
use ferry_core::tcp::{self, Connection, Listener, PairedConnection, Pending};
use zeroize::Zeroize;

use crate::errors::{bad_config, failed, from_noise, from_path, from_peer, from_tcp};
use crate::guard::GuardedFs;
use crate::state::{
    Candidate, DeviceLive, HeldPairing, Pairing, State, TransferRow, UsbForward, hex_of,
    key_from_hex, lock, now_unix_secs,
};
use crate::transfer;
use crate::{
    Config, DeviceInfo, EngineListener, FerryError, KeyPair, PairingCandidate, PairingState,
    TransferInfo, TransferState, Transport,
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

/// How long the discovery loop waits for one mDNS event before looking at
/// the stop flag again.
const BROWSE_TICK: Duration = Duration::from_millis(400);

/// How many addresses discovery keeps to try later.
const MAX_DISCOVERED: usize = 16;

/// Everything the engine's threads share.
pub(crate) struct Shared {
    /// Where change notifications go.
    pub(crate) listener: Arc<dyn EngineListener>,
    /// This device's long-lived key.
    pub(crate) key: StaticKey,
    /// The name sent in `hello`.
    pub(crate) display_name: String,
    /// Where transfer records live.
    pub(crate) transfers_dir: PathBuf,
    /// The folder served to paired devices.
    pub(crate) root: PathBuf,
    /// The port to bind, or zero for any free port.
    pub(crate) listen_port: u16,
    /// Everything mutable.
    pub(crate) state: Mutex<State>,
    /// Wakes every thread that is waiting, so `stop` does not wait out a
    /// sleep.
    pub(crate) wake: Condvar,
    /// Set by `stop`. Every loop checks it.
    pub(crate) stopping: Arc<AtomicBool>,
    /// The shared root, open, once `start` has run.
    pub(crate) fs: Mutex<Option<Arc<LocalFs>>>,
    /// The bound listener, once `start` has run.
    pub(crate) net: Mutex<Option<Arc<Listener>>>,
    /// The mDNS announcement, while this device is reachable.
    pub(crate) advertiser: Mutex<Option<Advertiser>>,
    /// The `adb` binary, when this machine has one.
    pub(crate) adb: Option<Adb>,
    /// Threads `stop` joins.
    pub(crate) joins: Mutex<Vec<JoinHandle<()>>>,
    /// How long pairing runs before it gives up.
    pub(crate) pairing_timeout: Mutex<Duration>,
}

impl Shared {
    /// True once `stop` has begun.
    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Wait up to `how_long`, or until `stop` wakes every thread.
    ///
    /// Returns false when the engine is stopping, so a loop can end.
    pub(crate) fn rest(&self, how_long: Duration) -> bool {
        let guard = lock(&self.state);
        let (guard, _) = self
            .wake
            .wait_timeout(guard, how_long)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(guard);
        !self.stopping()
    }

    /// Keep a thread so `stop` can join it, and drop handles that already
    /// finished.
    pub(crate) fn keep(&self, handle: JoinHandle<()>) {
        let mut joins = lock(&self.joins);
        joins.retain(|h| !h.is_finished());
        joins.push(handle);
    }

    /// The shared root, if `start` has opened it.
    pub(crate) fn shared_fs(&self) -> Option<Arc<LocalFs>> {
        lock(&self.fs).clone()
    }

    /// Where one transfer's record is stored.
    pub(crate) fn record_path(&self, id: &str) -> PathBuf {
        self.transfers_dir.join(format!("{id}.bin"))
    }

    /// Report a pairing state to the app and remember it.
    ///
    /// The lock is dropped before the callback runs. A listener that calls
    /// back into the engine would otherwise deadlock.
    pub(crate) fn set_pairing(&self, next: &PairingState) {
        {
            let mut state = lock(&self.state);
            state.pairing.shown = next.clone();
            if !state.pairing.is_running() {
                state.pairing.deadline = None;
                state.pairing.held = None;
                state.pairing.candidates.clear();
            }
        }
        self.listener.pairing_changed(next.clone());
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
    /// # Errors
    ///
    /// Returns `Runtime::BadConfig` when a directory cannot be made or the
    /// device list cannot be read, `Runtime::NameTooLong` when the display
    /// name is over 64 bytes, and a `NoiseError` code when the key is not
    /// two lots of 32 bytes.
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

        let data_dir = PathBuf::from(&config.data_dir);
        std::fs::create_dir_all(&data_dir)
            .map_err(|_| bad_config("The engine's own folder could not be made."))?;
        let transfers_dir = data_dir.join("transfers");
        std::fs::create_dir_all(&transfers_dir)
            .map_err(|_| bad_config("The transfers folder could not be made."))?;

        let peers = PeerStore::load(&data_dir.join("peers.bin"))
            .map_err(|_| bad_config("The paired device list could not be read."))?;

        let shared = Arc::new(Shared {
            listener: Arc::from(listener),
            key,
            display_name: config.display_name.clone(),
            transfers_dir,
            root: PathBuf::from(&config.shared_root),
            listen_port: config.listen_port,
            state: Mutex::new(State::new(peers)),
            wake: Condvar::new(),
            stopping: Arc::new(AtomicBool::new(false)),
            fs: Mutex::new(None),
            net: Mutex::new(None),
            advertiser: Mutex::new(None),
            adb: find_adb().map(Adb::new),
            joins: Mutex::new(Vec::new()),
            pairing_timeout: Mutex::new(PAIRING_TIMEOUT),
        });

        load_saved_transfers(&shared);
        Ok(Arc::new(Self { shared }))
    }

    /// Open the shared root, bind the listener, and start every loop.
    ///
    /// A machine with no `adb` is not an error. USB is simply unavailable.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::BadConfig` with a detail saying which part failed.
    pub fn start(&self) -> Result<(), FerryError> {
        if lock(&self.shared.state).started {
            return Ok(());
        }

        let fs = LocalFs::open(&self.shared.root)
            .map_err(|_| bad_config("The shared folder could not be opened."))?;
        let addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            self.shared.listen_port,
        ));
        let net = Listener::bind(addr)
            .map_err(|_| bad_config("The network port could not be opened."))?;
        let local_addr = net.local_addr();

        *lock(&self.shared.fs) = Some(Arc::new(fs));
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

        transfer::resume_all(&self.shared);
        Ok(())
    }

    /// Stop everything and join every loop. Safe to call twice.
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
        *lock(&self.shared.advertiser) = None;
        self.shared.wake.notify_all();

        // The accept loop is blocked inside `accept`. A connection to our own
        // port is the only way to bring it back, since the listener has no
        // way to be woken.
        wake_the_listener(&self.shared);

        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.shared.joins));
        for handle in handles {
            // A thread that already panicked has nothing left to report to.
            drop(handle.join());
        }

        self.remove_forwards();
        *lock(&self.shared.net) = None;
    }

    /// Advertise over mDNS and accept connections, or stop doing both.
    ///
    /// Turning this off does not close connections that are already serving.
    /// New ones are refused as soon as they are accepted.
    pub fn set_reachable(&self, on: bool) {
        let port = lock(&self.shared.state).listen_addr.map(|a| a.port());
        if on {
            if let Some(port) = port {
                // A network that refuses multicast still allows the cable and
                // a known address, so this failure does not stop the switch.
                *lock(&self.shared.advertiser) = Advertiser::start(port).ok();
            }
        } else {
            *lock(&self.shared.advertiser) = None;
        }
        lock(&self.shared.state).reachable = on;
        self.shared.listener.devices_changed();
    }

    /// Every paired device, with what is known about it right now.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceInfo> {
        lock(&self.shared.state).devices()
    }

    /// Forget a device: remove its key and every transfer record for it.
    ///
    /// A connection that is already serving this device stops answering at
    /// once, though its socket stays open until the peer goes away.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when no device has that key.
    pub fn forget(&self, key_hex: String) -> Result<(), FerryError> {
        let key = key_from_hex(&key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        let gone: Vec<String>;
        {
            let mut state = lock(&self.shared.state);
            if state.peers.remove(&key).is_none() {
                return Err(failed("Runtime::NotPaired"));
            }
            state.peers.save().map_err(|e| from_peer(&e))?;
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
        }
        for id in &gone {
            // A record that is already gone is the outcome asked for.
            drop(std::fs::remove_file(self.shared.record_path(id)));
        }
        self.shared.listener.devices_changed();
        self.shared.listener.transfers_changed();
        Ok(())
    }

    /// Enter pairing. Times out after two minutes.
    ///
    /// The Mac browses and polls `adb`, and reports candidates. The phone
    /// waits for one pairing handshake and reports the code. Calling this
    /// while a pairing is already running only reports the current state
    /// again.
    pub fn start_pairing(&self) {
        let timeout = *lock(&self.shared.pairing_timeout);
        {
            let mut state = lock(&self.shared.state);
            if state.pairing.is_running() {
                let shown = state.pairing.shown.clone();
                drop(state);
                self.shared.listener.pairing_changed(shown);
                return;
            }
            state.pairing = Pairing::idle();
            state.pairing.deadline = Some(Instant::now() + timeout);
        }
        self.shared.set_pairing(&PairingState::Waiting);

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || pairing_watchdog(&shared)));
    }

    /// Dial the chosen candidate and run the pairing handshake.
    ///
    /// The dial happens on its own thread, so this returns at once. The code
    /// arrives through the listener.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NoCandidate` when that candidate is not listed, and
    /// `Runtime::PairingBusy` when a code is already showing.
    pub fn pick_candidate(&self, id: String) -> Result<(), FerryError> {
        let addr = {
            let state = lock(&self.shared.state);
            if !state.pairing.is_running() {
                return Err(failed("Runtime::NoCandidate"));
            }
            if !state.pairing.is_open_to_pairing() {
                return Err(failed("Runtime::PairingBusy"));
            }
            state
                .pairing
                .candidates
                .get(&id)
                .map(|c| c.addr)
                .ok_or_else(|| failed("Runtime::NoCandidate"))?
        };

        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || dial_for_pairing(&shared, addr)));
        Ok(())
    }

    /// Accept or reject the device whose code is showing.
    ///
    /// Accepting stores the peer and exchanges names. That takes a round
    /// trip, so it runs on its own thread and reports through the listener.
    pub fn confirm_pairing(&self, accept: bool) {
        let held = lock(&self.shared.state).pairing.held.take();
        let Some(held) = held else {
            if !accept {
                self.shared.set_pairing(&PairingState::Idle);
            }
            return;
        };
        if !accept {
            drop(held);
            self.shared.set_pairing(&PairingState::Idle);
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.shared
            .keep(std::thread::spawn(move || finish_pairing(&shared, held)));
    }

    /// Stop pairing and drop whatever it was holding.
    pub fn cancel_pairing(&self) {
        lock(&self.shared.state).pairing.held = None;
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
    pub fn pull(
        &self,
        device_key_hex: String,
        remote_path: String,
        local_name: String,
    ) -> Result<String, FerryError> {
        let source = RemotePath::parse(&remote_path).map_err(from_path)?;
        let destination = RemotePath::parse(&local_name).map_err(from_path)?;
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
                },
            );
            id
        };

        self.shared.listener.transfers_changed();
        transfer::spawn(&self.shared, &id);
        Ok(id)
    }

    /// Restart a failed transfer from its resume point.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::TransferNotFound` when no transfer has that
    /// identifier.
    pub fn retry(&self, transfer_id: String) -> Result<(), FerryError> {
        {
            let mut state = lock(&self.shared.state);
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
        }
        self.shared.listener.transfers_changed();
        transfer::spawn(&self.shared, &transfer_id);
        Ok(())
    }
}

impl Engine {
    /// Inject a Wi-Fi candidate, as if discovery had found it.
    ///
    /// This is the seam the integration test uses, so pairing can be proved
    /// without a working mDNS network. It is not exported to the apps.
    #[doc(hidden)]
    pub fn offer_candidate(&self, addr: SocketAddr) {
        add_candidate(&self.shared, &wifi_candidate(addr), addr);
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

/// The last component of a path, for display.
fn leaf_of(path: &RemotePath) -> String {
    path.components().last().unwrap_or(path.as_str()).to_owned()
}

/// Connect to our own listener so a blocked `accept` returns.
fn wake_the_listener(shared: &Shared) {
    let Some(addr) = lock(&shared.state).listen_addr else {
        return;
    };
    let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, addr.port()));
    // The connection is dropped at once. Its only job is to end the wait.
    drop(TcpStream::connect_timeout(&local, Duration::from_secs(2)));
}

/// Read every transfer record left by an earlier run, as paused.
///
/// This is what makes a transfer survive the app closing. See job 3.
fn load_saved_transfers(shared: &Arc<Shared>) {
    let Ok(entries) = std::fs::read_dir(&shared.transfers_dir) else {
        return;
    };
    let mut state = lock(&shared.state);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let Some(key_hex) = id.split_once('-').map(|(key, _)| key.to_owned()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(record) = Transfer::decode(&bytes) else {
            continue;
        };
        state.transfers.insert(
            id.clone(),
            TransferRow {
                id,
                device_key_hex: key_hex,
                file_name: leaf_of(&record.destination),
                source: record.source,
                destination: record.destination,
                bytes_total: record.manifest.length(),
                bytes_done: 0,
                state: TransferState::Paused,
                transport: None,
                error: None,
                source_size: None,
                source_mtime: None,
                running: false,
            },
        );
    }
}

/// Accept connections until the engine stops.
fn accept_loop(shared: &Arc<Shared>, net: &Arc<Listener>) {
    while !shared.stopping() {
        let Ok(pending) = net.accept() else {
            // Too many handshakes at once, or a socket error. Either way the
            // next connection is the one that matters.
            continue;
        };
        if shared.stopping() {
            drop(pending);
            continue;
        }
        let welcome = {
            let state = lock(&shared.state);
            state.reachable || state.pairing.is_open_to_pairing()
        };
        if !welcome {
            // This is what `set_reachable(false)` means. The connection is
            // dropped after accept, not before, because a listener cannot
            // refuse before accepting.
            drop(pending);
            continue;
        }
        let shared = Arc::clone(shared);
        // A serving thread is not joined. See the crate documentation.
        drop(std::thread::spawn(move || handle_inbound(&shared, pending)));
    }
}

/// Decide what one accepted connection is, and run it.
fn handle_inbound(shared: &Arc<Shared>, pending: Pending) {
    let remote = pending.remote();
    let pair_this_one = lock(&shared.state).pairing.is_open_to_pairing();
    if pair_this_one {
        accept_pairing(shared, pending, remote);
        return;
    }

    let Some(peer) = choose_peer(shared, remote) else {
        drop(pending);
        return;
    };
    // A refused handshake is the design working: whoever called does not
    // hold a key this device paired with. Nothing to report.
    if let Ok(connection) = pending.connect(&shared.key, &peer) {
        serve_connection(shared, connection, peer);
    }
}

/// Run the pairing handshake as the side that accepted the connection.
fn accept_pairing(shared: &Arc<Shared>, pending: Pending, remote: SocketAddr) {
    match pending.pair(&shared.key) {
        Ok(connection) => hold_pairing(shared, connection, remote, true),
        Err(error) => report_pairing_failure(shared, &error),
    }
}

/// Report a failed handshake, unless pairing has already moved on.
///
/// Someone who cancelled while the handshake ran must not see a failure for
/// a pairing they already stopped.
fn report_pairing_failure(shared: &Arc<Shared>, error: &ferry_core::tcp::TcpError) {
    if !lock(&shared.state).pairing.is_running() {
        return;
    }
    shared.set_pairing(&PairingState::Failed {
        error: from_tcp(error),
    });
}

/// Dial one candidate and run the pairing handshake as the initiator.
fn dial_for_pairing(shared: &Arc<Shared>, addr: SocketAddr) {
    match tcp::pair(addr, &shared.key) {
        Ok(connection) => hold_pairing(shared, connection, addr, false),
        Err(error) => report_pairing_failure(shared, &error),
    }
}

/// Show the code and hold the connection until someone confirms.
fn hold_pairing(
    shared: &Arc<Shared>,
    connection: PairedConnection,
    addr: SocketAddr,
    accepted: bool,
) {
    let code = connection.paired.code.to_string();
    {
        let mut state = lock(&shared.state);
        if !state.pairing.is_running() {
            // Pairing was cancelled while the handshake ran. Nothing is
            // stored, so the connection goes away with this value.
            return;
        }
        state.pairing.held = Some(HeldPairing {
            connection,
            addr,
            accepted,
        });
    }
    shared.set_pairing(&PairingState::Code { code });
}

/// Exchange names, store the peer, and report the new device.
fn finish_pairing(shared: &Arc<Shared>, held: HeldPairing) {
    let HeldPairing {
        connection,
        addr,
        accepted,
    } = held;
    let peer_key = connection.paired.peer;
    let mut stream = connection.paired.stream;

    let name = match exchange_hello(&mut stream, &shared.display_name) {
        Ok(name) => name,
        Err(error) => {
            shared.set_pairing(&PairingState::Failed {
                error: crate::errors::from_rpc(&error),
            });
            return;
        }
    };

    let key_hex = hex_of(&peer_key);
    let device = {
        let mut state = lock(&shared.state);
        state.peers.add(Peer {
            key: peer_key,
            name: name.clone(),
            paired_unix_secs: now_unix_secs(),
        });
        if let Err(error) = state.peers.save() {
            state.peers.remove(&peer_key);
            let error = from_peer(&error);
            drop(state);
            shared.set_pairing(&PairingState::Failed { error });
            return;
        }
        let live = state.live_mut(&key_hex);
        live.last_addr = Some(addr);
        live.last_seen_unix_secs = Some(now_unix_secs());
        state.device(&key_hex)
    };

    let Some(device) = device else {
        shared.set_pairing(&PairingState::Failed {
            error: failed("Runtime::NotPaired"),
        });
        return;
    };
    shared.set_pairing(&PairingState::Confirmed { device });
    shared.listener.devices_changed();

    if accepted {
        // The side that accepted keeps serving on this stream. The side that
        // dialed lets it go here: two servers on one stream would each wait
        // for the other to speak.
        //
        // Names were exchanged a few lines above, so this serves the stream
        // as it stands. A second hello would sit waiting for one the other
        // side already sent.
        serve_named_stream(
            shared,
            stream,
            peer_key,
            transport_for_inbound(shared, addr),
            name,
        );
    }
}

/// Give up on pairing once the deadline passes.
fn pairing_watchdog(shared: &Arc<Shared>) {
    loop {
        let deadline = lock(&shared.state).pairing.deadline;
        let Some(deadline) = deadline else {
            return;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if !shared.rest(left) {
            return;
        }
        if !lock(&shared.state).pairing.is_running() {
            return;
        }
    }
    if lock(&shared.state).pairing.is_running() {
        shared.set_pairing(&PairingState::Failed {
            error: failed("Runtime::PairingTimeout"),
        });
    }
}

/// Which stored peer an inbound connection is most likely to be.
///
/// The wire says nothing about who is calling before the handshake, and a
/// handshake cannot be retried on one stream. See the crate documentation for
/// what this costs and what the real fix is.
fn choose_peer(shared: &Arc<Shared>, remote: SocketAddr) -> Option<PublicKey> {
    let state = lock(&shared.state);
    let peers = state.peers.all();
    if peers.len() == 1 {
        return peers.first().map(|p| p.key);
    }
    let by_address = peers.iter().find(|peer| {
        state
            .live
            .get(&hex_of(&peer.key))
            .and_then(|live| live.last_addr)
            .is_some_and(|addr| addr.ip() == remote.ip())
    });
    by_address.or_else(|| peers.first()).map(|peer| peer.key)
}

/// A connection arriving on the loopback address came through an `adb`
/// forward, unless this machine is the one that made the forward.
///
/// Only the Mac runs `adb`. So a loopback connection into a device with no
/// `adb` is the cable, and a loopback connection into a machine that has
/// `adb` is another program on the same machine.
fn transport_for_inbound(shared: &Arc<Shared>, remote: SocketAddr) -> Transport {
    if remote.ip().is_loopback() && shared.adb.is_none() {
        Transport::Usb
    } else {
        Transport::Wifi
    }
}

/// Exchange names, then serve the shared root until the connection ends.
fn serve_connection(shared: &Arc<Shared>, connection: Connection, peer: PublicKey) {
    let transport = transport_for_inbound(shared, connection.remote);
    lock(&shared.state)
        .live_mut(&hex_of(&peer))
        .last_addr
        .replace(connection.remote);
    serve_stream(shared, connection.stream, peer, transport);
}

/// Serve the shared root on one stream, and keep the device list honest.
fn serve_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
) {
    let Ok(name) = exchange_hello(&mut stream, &shared.display_name) else {
        return;
    };
    serve_named_stream(shared, stream, peer, transport, name);
}

/// Serve the shared root on a stream whose names were already exchanged.
fn serve_named_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    name: String,
) {
    let key_hex = hex_of(&peer);
    let allowed = Arc::new(AtomicBool::new(true));
    {
        let mut state = lock(&shared.state);
        // A name the peer changed since pairing is stored, so the list stays
        // current without another pairing.
        if let Some(stored) = state.peers.get(&peer)
            && stored.name != name
        {
            let paired_unix_secs = stored.paired_unix_secs;
            state.peers.add(Peer {
                key: peer,
                name,
                paired_unix_secs,
            });
            drop(state.peers.save());
        }
        let live = state.live_mut(&key_hex);
        live.reachable_via = Some(transport);
        live.serving.push(Arc::clone(&allowed));
    }
    shared.listener.devices_changed();

    let Some(fs) = shared.shared_fs() else {
        return;
    };
    let guarded = GuardedFs::new(fs, Arc::clone(&allowed));
    // A connection that ends is the ordinary outcome. The error, if any, has
    // nowhere useful to go: the person did not ask for this connection.
    drop(serve(&mut stream, &guarded));

    {
        let mut state = lock(&shared.state);
        let live = state.live_mut(&key_hex);
        live.reachable_via = None;
        live.last_seen_unix_secs = Some(now_unix_secs());
        live.serving.retain(|switch| !Arc::ptr_eq(switch, &allowed));
    }
    shared.listener.devices_changed();
}

/// Watch mDNS for the whole session.
fn browse_loop(shared: &Arc<Shared>) {
    let Ok(browser) = Browser::start() else {
        // A network that refuses multicast leaves the cable and a known
        // address, both of which work without this loop.
        return;
    };
    while !shared.stopping() {
        match browser.next(BROWSE_TICK) {
            Some(Event::Found {
                instance,
                addr,
                version: _,
            }) => on_discovered(shared, &instance, addr),
            Some(Event::Lost { instance }) => {
                lock(&shared.state)
                    .pairing
                    .candidates
                    .remove(&format!("wifi:{instance}"));
            }
            None => {}
        }
    }
}

/// Record an address discovery found, and offer it while pairing.
fn on_discovered(shared: &Arc<Shared>, instance: &str, addr: SocketAddr) {
    {
        let mut state = lock(&shared.state);
        state.discovered.retain(|known| *known != addr);
        state.discovered.insert(0, addr);
        state.discovered.truncate(MAX_DISCOVERED);
    }
    let shown = PairingCandidate {
        id: format!("wifi:{instance}"),
        transport: Transport::Wifi,
        short_code: last_four(instance),
    };
    add_candidate(shared, &shown, addr);
}

/// The candidate an injected address becomes.
fn wifi_candidate(addr: SocketAddr) -> PairingCandidate {
    let text = addr.to_string();
    PairingCandidate {
        id: format!("wifi:{text}"),
        transport: Transport::Wifi,
        short_code: last_four(&text),
    }
}

/// The last four characters, which is what both screens show.
fn last_four(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

/// Put one candidate in front of the person, if pairing is still open.
fn add_candidate(shared: &Arc<Shared>, shown: &PairingCandidate, addr: SocketAddr) {
    let candidates = {
        let mut state = lock(&shared.state);
        if !state.pairing.is_open_to_pairing() {
            return;
        }
        state.pairing.candidates.insert(
            shown.id.clone(),
            Candidate {
                shown: shown.clone(),
                addr,
            },
        );
        state.candidate_list()
    };
    shared.set_pairing(&PairingState::Found { candidates });
}

/// Ask `adb` what is plugged in, every three seconds.
fn adb_loop(shared: &Arc<Shared>) {
    while !shared.stopping() {
        poll_adb_once(shared);
        if !shared.rest(ADB_POLL) {
            return;
        }
    }
}

/// One pass over the plugged in devices.
fn poll_adb_once(shared: &Arc<Shared>) {
    let Some(adb) = shared.adb.as_ref() else {
        return;
    };
    let Ok(serials) = adb.devices() else {
        return;
    };

    let known: Vec<UsbForward> = lock(&shared.state).forwards.clone();
    let mut current: Vec<UsbForward> = Vec::new();
    for serial in &serials {
        if let Some(existing) = known.iter().find(|f| f.serial == *serial) {
            current.push(existing.clone());
            continue;
        }
        if let Ok(local_port) = adb.forward(serial, 0, FERRY_PHONE_PORT) {
            current.push(UsbForward {
                serial: serial.clone(),
                local_port,
            });
        }
    }
    for gone in known.iter().filter(|f| !serials.contains(&f.serial)) {
        drop(adb.remove_forward(&gone.serial, gone.local_port));
    }
    lock(&shared.state).forwards.clone_from(&current);

    for forward in &current {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, forward.local_port));
        let shown = PairingCandidate {
            id: format!("usb:{}", forward.serial),
            transport: Transport::Usb,
            short_code: last_four(&forward.serial),
        };
        add_candidate(shared, &shown, addr);
    }
}

/// Every address worth trying for one device, best first.
///
/// USB comes first because a cable is the reliable path. See decision record
/// 9 and job 2.
pub(crate) fn dial_targets(shared: &Arc<Shared>, key_hex: &str) -> Vec<(SocketAddr, Transport)> {
    let state = lock(&shared.state);
    let mut targets: Vec<(SocketAddr, Transport)> = Vec::new();
    for forward in &state.forwards {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, forward.local_port));
        targets.push((addr, Transport::Usb));
    }
    if let Some(live) = state.live.get(key_hex)
        && let Some(addr) = live.last_addr
    {
        targets.push((addr, Transport::Wifi));
    }
    for addr in &state.discovered {
        targets.push((*addr, Transport::Wifi));
    }
    let mut seen: Vec<SocketAddr> = Vec::new();
    targets.retain(|(addr, _)| {
        if seen.contains(addr) {
            false
        } else {
            seen.push(*addr);
            true
        }
    });
    targets
}

/// Write down that a device is reachable, and by which path.
pub(crate) fn mark_reachable(
    shared: &Arc<Shared>,
    key_hex: &str,
    addr: SocketAddr,
    via: Transport,
) {
    let mut state = lock(&shared.state);
    let live: &mut DeviceLive = state.live_mut(key_hex);
    live.reachable_via = Some(via);
    live.last_addr = Some(addr);
    live.last_seen_unix_secs = Some(now_unix_secs());
    if via == Transport::Usb {
        live.usb_port = Some(addr.port());
    }
}
