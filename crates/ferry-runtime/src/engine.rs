//! The engine object and the threads it owns.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ferry_core::adb::{Adb, find_adb};
use ferry_core::chunk::ChunkSize;
use ferry_core::discovery::{Advertiser, Browser, Event};
use ferry_core::localfs::LocalFs;
use ferry_core::noise::{PublicKey, SecureStream, StaticKey};
use ferry_core::ops::FileKind;
use ferry_core::path::{PathError, RemotePath};
use ferry_core::peers::{DeviceKind, Peer, PeerStore};
use ferry_core::rpc::{Client, MAX_NAME_LEN, exchange_hello, serve};
use ferry_core::session::SessionId;
use ferry_core::tcp::{self, Connection, Listener, PairedConnection, Pending};
use zeroize::Zeroize;

use crate::errors::{
    bad_config, failed, from_chunk_size, from_noise, from_path, from_peer, from_rpc, from_tcp,
};
use crate::guard::{GuardedFs, StopAware};
use crate::notify::{Change, Notify};
use crate::record::{Record, read_record};
use crate::state::{
    Candidate, DeviceLive, HeldPairing, Pairing, State, TransferRow, UsbForward, hex_of,
    key_from_hex, lock, now_unix_secs,
};
use crate::transfer::{self, BACKOFF_MIN};
use crate::{
    Config, DeviceInfo, Direction, EngineListener, Entry, EntryKind, FerryError, KeyPair,
    PairingCandidate, PairingState, Status, TransferInfo, TransferState, Transport,
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

/// Everything the engine's threads share.
pub(crate) struct Shared {
    /// Where change notifications go, while the engine is running.
    pub(crate) notify: Notify,
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
    /// The shortest wait before a transfer tries again. `BACKOFF_MIN` unless
    /// a test lowers it.
    pub(crate) backoff_min: Mutex<Duration>,
    /// The chunk size the next first pass uses. `ChunkSize::one_mebibyte()`
    /// unless a test changes it.
    pub(crate) chunk_size: Mutex<ChunkSize>,
    /// The byte count the next dial fails at, if a test armed one.
    ///
    /// Taken, and cleared, by the dial that carries it, so only that one dial
    /// is cut.
    pub(crate) cut: Mutex<Option<u64>>,
    /// Bytes moved on the wire since this engine started, across every dial.
    ///
    /// Counted by [`crate::guard::Cut`], whether or not a cut is armed. A
    /// test reads this back through [`Engine::wire_bytes`] to measure what a
    /// pull cost.
    pub(crate) wire_bytes: Arc<AtomicU64>,
    /// Held by whichever thread is writing the paired device list.
    ///
    /// Writing that list calls `fsync`, which is slow, so the state lock is
    /// dropped for it. This takes its place: one writer at a time, so two
    /// savers cannot write the file in one order and publish in the other.
    pub(crate) peers_write: Mutex<()>,
    /// The claim on the data folder, held for as long as the engine is.
    pub(crate) dir_lock: DirLock,
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
                state.pairing.deadline_unix_secs = None;
                state.pairing.held = None;
                state.pairing.dialing = false;
                state.pairing.candidates.clear();
            }
        }
        self.notify.pairing(next);
    }
}

/// The claim on one data folder.
///
/// Two engines on one folder each keep the whole paired device list in
/// memory and each write the whole file, so the second one to write puts
/// back what the first one removed. A forgotten device would come back. One
/// file, created with `create_new`, is what stops that: the second engine
/// cannot create it, so it is refused before it opens anything.
///
/// The file holds the process identifier, so a person can see which program
/// to close. A crash leaves the file behind, and the app is told which file
/// to offer to clear.
pub(crate) struct DirLock {
    path: PathBuf,
    held: AtomicBool,
}

impl DirLock {
    /// Claim the folder, or report that somebody else holds it.
    fn take(path: PathBuf) -> Result<Self, FerryError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match file {
            Ok(mut file) => {
                // The identifier is written for a person to read. Nothing
                // depends on it, so a failed write is not a failed claim.
                drop(std::io::Write::write_all(
                    &mut file,
                    format!("{}\n", std::process::id()).as_bytes(),
                ));
                Ok(Self {
                    path,
                    held: AtomicBool::new(true),
                })
            }
            Err(_) => Err(bad_config(&format!(
                "Another Ferry is using this folder, or a crash left {} behind.",
                path.display()
            ))),
        }
    }

    /// Give the folder back. Doing this twice is safe.
    pub(crate) fn release(&self) {
        if self.held.swap(false, Ordering::SeqCst) {
            // A file that is already gone is the outcome asked for.
            drop(std::fs::remove_file(&self.path));
        }
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Note that something changed, and tell the app when its turn comes.
///
/// Every callback but a pairing state goes through here. Without it a
/// hundred transfers moving at once would call the app on every chunk.
pub(crate) fn notify(shared: &Arc<Shared>, change: Change) {
    shared.notify.mark(change);
    report_due(shared, false);
    if shared.notify.claim_timer() && !shared.stopping() {
        let shared_for_thread = Arc::clone(shared);
        shared.keep(std::thread::spawn(move || notify_timer(&shared_for_thread)));
    }
}

/// Tell the app about every kind whose turn has come.
fn report_due(shared: &Arc<Shared>, force: bool) {
    let due = shared.notify.take_due(force);
    if due.is_empty() {
        return;
    }
    if due.found {
        let found = {
            let state = lock(&shared.state);
            // A list that nobody is picking from is not worth reporting.
            state
                .pairing
                .is_open_to_pairing()
                .then(|| (state.candidate_list(), state.pairing.deadline_unix_secs))
        };
        if let Some((candidates, expires_unix_secs)) = found {
            shared.set_pairing(&PairingState::Found {
                candidates,
                expires_unix_secs: expires_unix_secs.unwrap_or_else(now_unix_secs),
            });
        }
    }
    shared.notify.send(due);
}

/// Wait for the turn of whatever is held back, then report it.
///
/// One of these runs at a time. It ends when nothing is waiting, and it
/// reports what is left before it ends, so the last state always reaches the
/// app.
fn notify_timer(shared: &Arc<Shared>) {
    loop {
        let Some(left) = shared.notify.next_turn() else {
            return;
        };
        if !left.is_zero() {
            shared.rest(left);
        }
        if shared.stopping() {
            // `stop` joins this thread before it empties the listener slot,
            // so this last report still reaches the app.
            report_due(shared, true);
            shared.notify.release_timer();
            return;
        }
        report_due(shared, false);
    }
}

/// Change the paired device list and write it out.
///
/// The state lock is not held across the write, because writing calls
/// `fsync`. A peer that sends a new name would otherwise stop every other
/// thread for as long as the disk takes.
///
/// # Errors
///
/// Returns a `PeerError` code when the list cannot be written. Nothing is
/// published in that case, so memory and disk still agree.
pub(crate) fn save_peers(
    shared: &Arc<Shared>,
    change: impl FnOnce(&mut PeerStore),
) -> Result<(), FerryError> {
    let writing = lock(&shared.peers_write);
    let mut copy = lock(&shared.state).peers.clone();
    change(&mut copy);
    let written = copy.save();
    if written.is_ok() {
        lock(&shared.state).peers = copy;
    }
    drop(writing);
    written.map_err(|e| from_peer(&e))
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
    /// device list cannot be read, or another engine is already using the
    /// directory, `Runtime::NameTooLong` when the display name is over 64
    /// bytes, and a `NoiseError` code when the key is not two lots of 32
    /// bytes.
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

        // A version 1 peer file predates the kind byte, so every peer in it
        // is assumed to be the opposite of this device: with one Mac and
        // one phone, that is always right. See `this_devices_kind`.
        let assumed_peer_kind = match this_devices_kind() {
            DeviceKind::Mac => DeviceKind::Phone,
            DeviceKind::Phone => DeviceKind::Mac,
        };
        let peers = PeerStore::load(&data_dir.join("peers.bin"), assumed_peer_kind)
            .map_err(|_| bad_config("The paired device list could not be read."))?;

        // Last, because nothing below it can fail and leave the claim behind.
        let dir_lock = DirLock::take(data_dir.join("lock"))?;

        let shared = Arc::new(Shared {
            notify: Notify::new(listener),
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
            backoff_min: Mutex::new(BACKOFF_MIN),
            chunk_size: Mutex::new(ChunkSize::one_mebibyte()),
            cut: Mutex::new(None),
            wire_bytes: Arc::new(AtomicU64::new(0)),
            peers_write: Mutex::new(()),
            dir_lock,
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
        *lock(&self.shared.advertiser) = None;
        self.shared.wake.notify_all();

        // A serving thread cannot be woken, so it is taken from instead:
        // every switch goes off and the shared root goes away. A connection
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
        *lock(&self.shared.fs) = None;

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
            mount: None,
        }
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
        save_peers(&self.shared, |store| {
            drop(store.remove(&key));
        })?;

        let gone: Vec<String>;
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
        }
        let mut trouble = None;
        for id in &gone {
            if let Err(error) = remove_record(&self.shared, id) {
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

    /// Enter pairing. Times out after two minutes.
    ///
    /// The Mac browses and polls `adb`, and reports candidates. The phone
    /// waits for one pairing handshake and reports the code. Calling this
    /// while a pairing is already running only reports the current state
    /// again.
    pub fn start_pairing(&self) {
        let timeout = *lock(&self.shared.pairing_timeout);
        let expires_unix_secs =
            now_unix_secs() + i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX);
        {
            let mut state = lock(&self.shared.state);
            if state.pairing.is_running() {
                let shown = state.pairing.shown.clone();
                drop(state);
                self.shared.notify.pairing(&shown);
                return;
            }
            state.pairing = Pairing::idle();
            state.pairing.deadline = Some(Instant::now() + timeout);
            state.pairing.deadline_unix_secs = Some(expires_unix_secs);
        }
        self.shared
            .set_pairing(&PairingState::Waiting { expires_unix_secs });

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
        {
            let mut state = lock(&self.shared.state);
            state.pairing.held = None;
            state.pairing.dialing = false;
        }
        self.shared.set_pairing(&PairingState::Idle);
    }

    /// Every transfer, as the app shows them.
    #[must_use]
    pub fn transfers(&self) -> Vec<TransferInfo> {
        let chunk_size = *lock(&self.shared.chunk_size);
        lock(&self.shared.state)
            .transfers
            .values()
            .map(|row| row.info(chunk_size))
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
                },
            );
            id
        };

        notify(&self.shared, Change::Transfers);
        transfer::spawn(&self.shared, &id);
        Ok(id)
    }

    /// List every entry in one folder on a paired device.
    ///
    /// Dials the device, then pages through the server's cursor until it
    /// reports no more entries, and returns them in the order the server
    /// sent them. This blocks for one round trip per page, so the app must
    /// call it off the main thread.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when the path is refused,
    /// `Runtime::NotPaired` when that device is not stored,
    /// `Runtime::NotStarted` before [`Engine::start`] has run,
    /// `Runtime::NotReachable` when no dial succeeds, and an `OpError` code
    /// when the peer refuses, such as `OpError::NotFound` for a folder that
    /// does not exist.
    pub fn list(
        &self,
        device_key_hex: String,
        remote_path: String,
    ) -> Result<Vec<Entry>, FerryError> {
        let path = RemotePath::parse(&remote_path).map_err(from_path)?;
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

        let (stream, addr, via) = transfer::dial(&self.shared, &device_key_hex, &key)?;
        mark_reachable(&self.shared, &device_key_hex, addr, via);

        let mut stream = StopAware::new(stream, Arc::clone(&self.shared.stopping));
        exchange_hello(&mut stream, &self.shared.display_name, this_devices_kind())
            .map_err(|error| from_rpc(&error))?;

        let mut client = Client::new(stream);
        let mut entries = Vec::new();
        let mut cursor = 0u64;
        loop {
            let (page, next_cursor) = client
                .list(&path, cursor)
                .map_err(|error| from_rpc(&error))?;
            entries.extend(page.into_iter().map(entry_from_core));
            let Some(next_cursor) = next_cursor else {
                break;
            };
            cursor = next_cursor;
        }
        Ok(entries)
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

/// This device's own kind, for the hello payload and for the assumed kind
/// of a version 1 peer file.
///
/// `Config` has no `kind` field yet, so this guesses from the target OS:
/// every build of this engine today is either the Mac app or the Android
/// app, and never both.
// TODO(engine 11): replaced by Config.kind on main.
pub(crate) fn this_devices_kind() -> DeviceKind {
    if cfg!(target_os = "macos") {
        DeviceKind::Mac
    } else {
        DeviceKind::Phone
    }
}

/// The last component of a path, for display.
fn leaf_of(path: &RemotePath) -> String {
    path.components().last().unwrap_or(path.as_str()).to_owned()
}

/// Turn one core entry into the shape the app receives.
fn entry_from_core(entry: ferry_core::ops::Entry) -> Entry {
    Entry {
        name: entry.name,
        kind: match entry.kind {
            FileKind::File => EntryKind::File,
            FileKind::Directory => EntryKind::Directory,
        },
        size: entry.size,
        modified_unix_secs: entry.modified_unix_secs,
    }
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

/// Delete one transfer's record, and say so when it cannot be done.
///
/// A record that stays behind is not a small thing. Every start reads the
/// records folder, so the transfer would come back from the dead.
///
/// # Errors
///
/// Returns `TransferError::Local` when the file is there and will not go.
pub(crate) fn remove_record(shared: &Arc<Shared>, id: &str) -> Result<(), FerryError> {
    match std::fs::remove_file(shared.record_path(id)) {
        Ok(()) => Ok(()),
        // A record that is already gone is the outcome asked for.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(failed("TransferError::Local")),
    }
}

/// Read every transfer record left by an earlier run, as paused.
///
/// This is what makes a transfer survive the app closing. See job 3. A
/// record written during a first pass comes back with the bytes that pass
/// had already verified, so the file is not fetched again from the start.
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
        let Ok(Some(record)) = read_record(&path) else {
            continue;
        };
        state
            .transfers
            .insert(id.clone(), row_from_record(id, key_hex, &record));
    }
}

/// The row one stored record becomes.
fn row_from_record(id: String, key_hex: String, record: &Record) -> TransferRow {
    let (source, destination, bytes_total, bytes_done, source_size, source_mtime, meta) =
        match record {
            Record::FirstPass(meta, pass) => (
                pass.source.clone(),
                pass.destination.clone(),
                pass.source_size,
                pass.bytes_done(),
                Some(pass.source_size),
                Some(pass.source_mtime),
                meta,
            ),
            // A ready record's bytes are counted again by the resume itself,
            // which reads the partial file back and hashes it.
            Record::Ready(meta, transfer) => (
                transfer.source.clone(),
                transfer.destination.clone(),
                transfer.manifest.length(),
                0,
                None,
                None,
                meta,
            ),
        };
    TransferRow {
        id,
        device_key_hex: key_hex,
        file_name: leaf_of(&destination),
        source,
        destination,
        bytes_total,
        bytes_done,
        state: TransferState::Paused,
        transport: None,
        error: None,
        source_size,
        source_mtime,
        running: false,
        attempt_after: None,
        backoff: BACKOFF_MIN,
        started_unix_secs: meta.started_unix_secs,
        // A row loaded from disk is about to be requeued by `resume_all`,
        // the same as an explicit `retry`, so its end time is cleared the
        // same way. A record this crate writes never carries `Some` here
        // regardless; see `Meta::ended_unix_secs`.
        ended_unix_secs: None,
        direction: meta.direction,
        speed_bytes_per_sec: None,
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

    // With no stored peer there is nobody this connection could be. The
    // handshake still runs, against a key nobody holds, so that a device
    // with no peer and a device with one peer look the same from outside.
    // Dropping the connection here instead would tell a stranger which of
    // the two this device is.
    let peer = choose_peer(shared, remote).unwrap_or_else(|| nobody(shared));
    // A refused handshake is the design working: whoever called does not
    // hold a key this device paired with. Nothing to report.
    if let Ok(connection) = pending.connect(&shared.key, &peer) {
        serve_connection(shared, connection, peer);
    }
}

/// A key no caller can hold the other half of.
///
/// A fresh key pair is thrown away as soon as its public half is taken, so
/// the handshake that follows cannot succeed. If the system gives no random
/// bytes, this device's own public key is used instead, which no caller
/// holds the private half of either.
fn nobody(shared: &Arc<Shared>) -> PublicKey {
    StaticKey::generate().map_or_else(|_| shared.key.public(), |key| key.public())
}

/// Run the pairing handshake as the side that accepted the connection.
fn accept_pairing(shared: &Arc<Shared>, pending: Pending, remote: SocketAddr) {
    match pending.pair(&shared.key) {
        // A refused hold means another pairing is already showing its code.
        // The person is looking at that one, so nothing is reported here.
        Ok(connection) => drop(hold_pairing(shared, connection, remote, true)),
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
        Ok(connection) => {
            // A refused hold means the pairing moved on while this dial ran.
            // The connection then goes away with the value.
            drop(hold_pairing(shared, connection, addr, false));
        }
        Err(error) => {
            lock(&shared.state).pairing.dialing = false;
            report_pairing_failure(shared, &error);
        }
    }
}

/// Show the code and hold the connection until someone confirms.
///
/// # Errors
///
/// Returns `Runtime::PairingBusy` when something is already held, or when
/// pairing has moved on. Nothing is shown in that case, because a second
/// code would replace the one the person is comparing.
fn hold_pairing(
    shared: &Arc<Shared>,
    connection: PairedConnection,
    addr: SocketAddr,
    accepted: bool,
) -> Result<(), FerryError> {
    let code = connection.paired.code.to_string();
    let expires_unix_secs;
    {
        let mut state = lock(&shared.state);
        if !accepted {
            // This dial is finished, whether or not its code is shown.
            state.pairing.dialing = false;
        }
        if !state.pairing.is_open_to_pairing() {
            return Err(failed("Runtime::PairingBusy"));
        }
        expires_unix_secs = state
            .pairing
            .deadline_unix_secs
            .unwrap_or_else(now_unix_secs);
        state.pairing.held = Some(HeldPairing {
            connection,
            addr,
            accepted,
        });
    }
    shared.set_pairing(&PairingState::Code {
        code,
        expires_unix_secs,
    });
    Ok(())
}

/// Report a failed pairing, unless pairing has already moved on.
fn fail_pairing(shared: &Arc<Shared>, error: FerryError) {
    if !lock(&shared.state).pairing.is_running() {
        return;
    }
    shared.set_pairing(&PairingState::Failed { error });
}

/// Exchange names, store the peer, and report the new device.
fn finish_pairing(shared: &Arc<Shared>, held: HeldPairing) {
    let HeldPairing {
        connection,
        addr,
        accepted,
    } = held;
    let peer_key = connection.paired.peer;

    let (name, kind, stream) = match hello_with_deadline(shared, connection.paired.stream) {
        Ok(triple) => triple,
        Err(error) => {
            fail_pairing(shared, error);
            return;
        }
    };

    // The watchdog may have given up while the names crossed. A pairing that
    // already reported Failed must not store a device or report Confirmed
    // after it.
    if !lock(&shared.state).pairing.is_running() {
        return;
    }

    let key_hex = hex_of(&peer_key);
    if let Err(error) = save_peers(shared, |store| {
        store.add(Peer {
            key: peer_key,
            name: name.clone(),
            paired_unix_secs: now_unix_secs(),
            kind,
        });
    }) {
        fail_pairing(shared, error);
        return;
    }

    let device = {
        let mut state = lock(&shared.state);
        let live = state.live_mut(&key_hex);
        live.last_addr = Some(addr);
        live.last_seen_unix_secs = Some(now_unix_secs());
        state.device(&key_hex)
    };

    let Some(device) = device else {
        fail_pairing(shared, failed("Runtime::NotPaired"));
        return;
    };
    if !lock(&shared.state).pairing.is_running() {
        return;
    }
    shared.set_pairing(&PairingState::Confirmed { device });
    notify(shared, Change::Devices);

    if accepted {
        // The side that accepted keeps serving on this stream. The side that
        // dialed lets it go here: two servers on one stream would each wait
        // for the other to speak.
        //
        // Names were exchanged a few lines above, so this serves the stream
        // as it stands. A second hello would sit waiting for one the other
        // side already sent.
        let allowed = register_serving(shared, &key_hex, addr);
        serve_named_stream(
            shared,
            stream,
            peer_key,
            transport_for_inbound(shared, addr),
            name,
            &allowed,
        );
    }
}

/// Exchange names on another thread, and give up after a short wait.
///
/// The other device may confirm slowly, or never. Its stream cannot be woken
/// from outside, so a name exchange that waits inside a read would hold this
/// thread until the idle timeout in `tcp.rs`, five minutes away. `stop`
/// joins this thread, so that was `stop`'s wait too. Running the exchange
/// beside this thread bounds both: the wait ends on its own deadline, and at
/// once when the engine stops.
///
/// The stream is wrapped so that the thread left behind ends quickly as
/// well, instead of holding a socket for five minutes.
///
/// # Errors
///
/// Returns an `RpcError` code when the exchange fails, and
/// `TcpError::Timeout` when it does not finish in time.
fn hello_with_deadline(
    shared: &Arc<Shared>,
    stream: SecureStream,
) -> Result<(String, DeviceKind, StopAware<SecureStream>), FerryError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let my_name = shared.display_name.clone();
    let stopping = Arc::clone(&shared.stopping);
    // Not joined. It ends when the exchange ends, or when the wrapper below
    // fails the next read because the engine is stopping.
    drop(std::thread::spawn(move || {
        let mut stream = StopAware::new(stream, stopping);
        let outcome = exchange_hello(&mut stream, &my_name, this_devices_kind())
            .map(|(name, kind)| (name, kind, stream));
        drop(sender.send(outcome));
    }));

    let deadline = Instant::now() + FINISH_PAIRING_DEADLINE;
    loop {
        match receiver.recv_timeout(FINISH_PAIRING_TICK) {
            Ok(Ok(triple)) => return Ok(triple),
            Ok(Err(error)) => return Err(from_rpc(&error)),
            // The thread went away without an answer, which only a panic
            // does. There is no name and no stream to carry on with.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(failed("FrameError::Io"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if shared.stopping() || Instant::now() >= deadline {
                    return Err(failed("TcpError::Timeout"));
                }
            }
        }
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
    // The held connection is taken out under the same lock that reads the
    // state, so a confirm that arrives a moment later finds nothing to
    // confirm.
    let running = {
        let mut state = lock(&shared.state);
        let running = state.pairing.is_running();
        state.pairing.held = None;
        state.pairing.dialing = false;
        running
    };
    if running {
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

/// Put this connection's off switch where `forget` and `stop` can reach it.
///
/// This happens as soon as the handshake proves who is calling, and before
/// the names are exchanged. A peer that finishes the handshake and then
/// holds its `hello` back for five minutes would otherwise be out of reach:
/// `forget` would find no switch to flip, and the connection would start
/// serving files afterwards.
///
/// The switch starts off if the engine is already stopping, so a connection
/// that arrives during `stop` serves nothing.
fn register_serving(shared: &Arc<Shared>, key_hex: &str, addr: SocketAddr) -> Arc<AtomicBool> {
    let allowed = Arc::new(AtomicBool::new(!shared.stopping()));
    let mut state = lock(&shared.state);
    let live = state.live_mut(key_hex);
    live.last_addr = Some(addr);
    live.serving.push(Arc::clone(&allowed));
    allowed
}

/// Exchange names, then serve the shared root until the connection ends.
fn serve_connection(shared: &Arc<Shared>, connection: Connection, peer: PublicKey) {
    let transport = transport_for_inbound(shared, connection.remote);
    let key_hex = hex_of(&peer);
    let allowed = register_serving(shared, &key_hex, connection.remote);
    serve_stream(shared, connection.stream, peer, transport, &allowed);
}

/// Serve the shared root on one stream, and keep the device list honest.
fn serve_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    allowed: &Arc<AtomicBool>,
) {
    let Ok((name, _kind)) = exchange_hello(&mut stream, &shared.display_name, this_devices_kind())
    else {
        release_serving(shared, &hex_of(&peer), allowed);
        return;
    };
    serve_named_stream(shared, stream, peer, transport, name, allowed);
}

/// Serve the shared root on a stream whose names were already exchanged.
fn serve_named_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    name: String,
    allowed: &Arc<AtomicBool>,
) {
    let key_hex = hex_of(&peer);
    let renamed = {
        let mut state = lock(&shared.state);
        // The handshake proved that the caller held a paired key at that
        // moment. `forget` may have run in the short gap before this
        // connection's switch was registered, and a switch registered after
        // that starts on. So the list is asked once more here, now that the
        // switch is in place for any later `forget` to flip.
        let Some(stored) = state.peers.get(&peer) else {
            drop(state);
            release_serving(shared, &key_hex, allowed);
            return;
        };
        let name_changed = stored.name != name;
        let paired_unix_secs = stored.paired_unix_secs;
        let kind = stored.kind;
        let live = state.live_mut(&key_hex);
        live.reachable_via = Some(transport);
        // A name the peer changed since pairing is stored, so the list stays
        // current without another pairing. Its kind does not change here: it
        // was set once, from the hello sent at pairing time.
        name_changed.then_some(Peer {
            key: peer,
            name,
            paired_unix_secs,
            kind,
        })
    };
    if let Some(peer) = renamed {
        // Outside the lock. Writing the list calls `fsync`, and any peer can
        // ask for this by sending a name of its own choosing.
        drop(save_peers(shared, |store| store.add(peer)));
    }
    notify(shared, Change::Devices);

    let Some(fs) = shared.shared_fs() else {
        release_serving(shared, &key_hex, allowed);
        return;
    };
    let guarded = GuardedFs::new(fs, Arc::clone(allowed));
    // A connection that ends is the ordinary outcome. The error, if any, has
    // nowhere useful to go: the person did not ask for this connection.
    drop(serve(&mut stream, &guarded));

    release_serving(shared, &key_hex, allowed);
    notify(shared, Change::Devices);
}

/// Take this connection's switch back once it has finished.
fn release_serving(shared: &Arc<Shared>, key_hex: &str, allowed: &Arc<AtomicBool>) {
    let mut state = lock(&shared.state);
    let live = state.live_mut(key_hex);
    live.reachable_via = None;
    live.last_seen_unix_secs = Some(now_unix_secs());
    live.serving.retain(|switch| !Arc::ptr_eq(switch, allowed));
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

/// Keep an address worth dialing later, newest first.
fn remember_address(shared: &Arc<Shared>, addr: SocketAddr) {
    let mut state = lock(&shared.state);
    state.discovered.retain(|known| *known != addr);
    state.discovered.insert(0, addr);
    state.discovered.truncate(MAX_DISCOVERED);
}

/// Record an address discovery found, and offer it while pairing.
fn on_discovered(shared: &Arc<Shared>, instance: &str, addr: SocketAddr) {
    remember_address(shared, addr);
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
///
/// The list is capped, and the change is reported at most once a second. The
/// whole list crosses the boundary with every report, so a network that
/// answers a thousand times must not become a thousand reports of a thousand
/// candidates each.
fn add_candidate(shared: &Arc<Shared>, shown: &PairingCandidate, addr: SocketAddr) {
    {
        let mut state = lock(&shared.state);
        if !state.pairing.is_open_to_pairing() {
            return;
        }
        let known = state.pairing.candidates.contains_key(&shown.id);
        if !known && state.pairing.candidates.len() >= MAX_CANDIDATES {
            return;
        }
        state.pairing.candidates.insert(
            shown.id.clone(),
            Candidate {
                shown: shown.clone(),
                addr,
            },
        );
    }
    notify(shared, Change::Found);
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
    } else {
        live.last_wifi_success_unix_secs = Some(now_unix_secs());
    }
}
