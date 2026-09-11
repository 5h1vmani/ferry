//! The engine object and the threads it owns.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ferry_core::adb::{Adb, find_adb};
use ferry_core::chunk::ChunkSize;
use ferry_core::discovery::{Advertiser, Browser, Event};
use ferry_core::limits::{MAX_READ_LEN, MAX_WRITE_LEN};
use ferry_core::localfs::LocalFs;
use ferry_core::noise::{NoiseError, PublicKey, QR_NONCE_LEN, SecureStream, StaticKey};
use ferry_core::offer::{Offer, PairingError as OfferError};
use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::{PathError, RemotePath};
// Aliased: `crate::DeviceKind` is the boundary enum `Config` and `DeviceInfo`
// carry; this is `ferry-core`'s own, which `hello` and `PeerStore` speak.
use ferry_core::peers::{DeviceKind as CoreDeviceKind, Peer, PeerError, PeerStore};
use ferry_core::roots::{RootSpec, Roots};
use ferry_core::rpc::{Client, MAX_NAME_LEN, exchange_hello, serve};
use ferry_core::session::SessionId;
use ferry_core::tcp::{
    self, Connection, IkPairedConnection, Listener, NegotiatedPending, PairedConnection, Pending,
    TcpError,
};
use ferry_core::version::Mode;
use zeroize::Zeroize;

use crate::access::{self, AccessLog, EntryFields, RollUp};
use crate::batch::{self, BatchRecord};
use crate::dav;
use crate::errors::{
    bad_config, failed, failed_with, from_chunk_size, from_noise, from_offer, from_op, from_path,
    from_peer, from_roots, from_rpc, from_tcp,
};
use crate::folder::{self, ListRecursiveError, RemoteLister};
use crate::guard::{AccessLogHandle, GuardedFs, RootsHandle, RootsState, StopAware};
use crate::networks;
use crate::notify::{Change, Notify};
use crate::pool::Pool;
use crate::push;
use crate::record::{Record, read_record};
use crate::state::{
    BatchRow, Candidate, DeviceLive, HeldPairing, Pairing, RequestedPairing, State, TransferRow,
    UsbForward, clear_gone_usb_forwards, hex_of, key_from_hex, lock, now_unix_secs,
};
use crate::transfer::{self, BACKOFF_MIN};
use crate::{
    AccessEntry, AccessVerb, Actor, AutoCopy, BatchInfo, Config, DeviceInfo, DeviceKind, Direction,
    EngineListener, Entry, EntryKind, FerryError, KeyPair, MountEndpoint, Origin, PairingCandidate,
    PairingMethod, PairingOffer, PairingState, Root, Status, TransferInfo, TransferState,
    Transport,
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

/// The sum of every listed file's size, saturating rather than overflowing.
fn total_listed_bytes(found: &[(RemotePath, u64)]) -> u64 {
    found
        .iter()
        .fold(0u64, |total, (_, size)| total.saturating_add(*size))
}

/// Build one `TransferRow`, still without a batch id, for each file
/// `list_recursive` found under a `pull_folder` call.
///
/// Every row is built before anything touches state, so a failure part way
/// through — only `SessionId::generate` starving of randomness can cause
/// one — leaves nothing behind, matching `pull_folder`'s "creates nothing"
/// rule on error.
fn rows_for_folder(
    found_files: &[(RemotePath, u64)],
    device_key_hex: &str,
    leaf: &str,
    prefix: &str,
    started_unix_secs: i64,
    chunk_size: ChunkSize,
) -> Result<Vec<TransferRow>, FerryError> {
    let mut rows = Vec::with_capacity(found_files.len());
    for (full_path, _size) in found_files {
        let relative = full_path
            .as_str()
            .strip_prefix(prefix)
            .unwrap_or(full_path.as_str());
        let destination = RemotePath::parse(&format!("{leaf}/{relative}")).map_err(from_path)?;
        let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        let id = format!("{device_key_hex}-{session}");
        let file_name = leaf_of(&destination);
        rows.push(TransferRow {
            id,
            device_key_hex: device_key_hex.to_owned(),
            file_name,
            source: full_path.clone(),
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
            started_unix_secs,
            ended_unix_secs: None,
            direction: Direction::Pull,
            speed_bytes_per_sec: None,
            // Filled in by the caller, once the batch id exists.
            batch_id: None,
            chunk_size,
        });
    }
    Ok(rows)
}

/// One access log entry, from the store's own type to the boundary's.
fn access_entry_from_core(entry: access::Entry) -> AccessEntry {
    AccessEntry {
        id: entry.id,
        device_key_hex: entry.device_key_hex,
        actor: entry.actor.into(),
        verb: entry.verb.into(),
        path: entry.path,
        bytes: entry.bytes,
        entries: entry.entries,
        files: entry.files,
        at_unix_secs: entry.at_unix_secs,
    }
}

/// Everything the engine's threads share.
pub(crate) struct Shared {
    /// Where change notifications go, while the engine is running.
    pub(crate) notify: Notify,
    /// This device's long-lived key.
    pub(crate) key: StaticKey,
    /// The name sent in `hello`.
    pub(crate) display_name: String,
    /// What kind of device this is. Sent in `hello`.
    pub(crate) kind: CoreDeviceKind,
    /// Where this engine keeps its own files. Read once, by `start`, which
    /// opens the access log under it.
    pub(crate) data_dir: PathBuf,
    /// Where transfer records live.
    pub(crate) transfers_dir: PathBuf,
    /// Where batch records live.
    pub(crate) batches_dir: PathBuf,
    /// The port to bind, or zero for any free port.
    pub(crate) listen_port: u16,
    /// Everything mutable.
    pub(crate) state: Mutex<State>,
    /// Wakes every thread that is waiting, so `stop` does not wait out a
    /// sleep.
    pub(crate) wake: Condvar,
    /// Set by `stop`. Every loop checks it.
    pub(crate) stopping: Arc<AtomicBool>,
    /// The served roots, open once `start` or `set_roots` has opened them.
    /// `None` before that, and again after `stop` clears it, the same way
    /// `stop` used to clear the one shared root.
    ///
    /// This is the handle every serving connection's [`GuardedFs`] shares,
    /// so a root change reaches every open connection on its next
    /// operation. See [`crate::guard::RootsHandle`].
    pub(crate) roots: RootsHandle,
    /// The roots as given to `new`, validated but not yet opened. `start`
    /// opens them. `Engine::roots` falls back to this before `start` runs.
    pub(crate) initial_roots: Vec<Root>,
    /// Where `download_dir` lives, as given to `new`. Read once, by
    /// `start`, which opens it.
    pub(crate) download_dir_config: PathBuf,
    /// The download folder, open once `start` has run. A pull writes here,
    /// never into a served root.
    pub(crate) download_fs: Mutex<Option<Arc<LocalFs>>>,
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
    /// Held by whichever thread is writing the trusted Wi-Fi network list.
    ///
    /// The same shape as [`Shared::peers_write`], and for the same reason:
    /// that file is written with `fsync` too, and no thread that wants
    /// `State` should wait for a disk.
    pub(crate) networks_write: Mutex<()>,
    /// The claim on the data folder, held for as long as the engine is.
    pub(crate) dir_lock: DirLock,
    /// The access log's roll-up, open once `start` has run and dropped
    /// again by `stop`. Held behind a mutex that a connection thread locks
    /// for exactly one `touch`: one buffered write to the day file, never a
    /// sync, so no operation on any connection ever waits on another
    /// connection's disk write (docs/engine-contract.md, item 13).
    pub(crate) access_log: AccessLogHandle,
    /// The next connection id handed to a served connection's `GuardedFs`,
    /// or to a calling-side operation's own roll-up entry. One counter for
    /// both sides, so two connections open at once never share an id.
    pub(crate) next_connection: AtomicU64,
    /// Every device's `WebDAV` bridge. `docs/engine-contract.md`, item 6.
    pub(crate) mounts: dav::MountRegistry,
    /// How many connections this engine has accepted and begun to serve.
    ///
    /// Counts up and never down. Only a test reads it, through
    /// [`Engine::accepted_connections`], and it reads a difference across
    /// the calls it makes rather than an absolute count.
    pub(crate) accepted: AtomicU64,
    /// One connection pool per device key, made on first use by
    /// [`Shared::pool_for`] and dropped by `forget` and by `stop`.
    ///
    /// `docs/engine-contract.md`, item 19: the bridge and [`Engine::list`]
    /// borrow from the same four connections to a device, so a Finder
    /// request and a file picker listing never open a socket each.
    pub(crate) pools: Mutex<HashMap<String, Arc<Pool>>>,
    /// A clone of every open transfer connection's raw socket, keyed by a
    /// connection id from the same counter as `next_connection`.
    ///
    /// A worker can be blocked in a kernel read deep inside an encrypted
    /// stream, where the stopping flag is invisible to it. `stop` calls
    /// `shutdown` on every socket registered here, right after it sets that
    /// flag, which turns a blocked read into an error at once instead of
    /// waiting for the peer or the idle timeout in `tcp.rs`.
    /// `docs/engine-contract.md` item 16c.
    pub(crate) sockets: Mutex<HashMap<u64, TcpStream>>,
    // ---- Item 14: automatic copying. See `auto_copy.rs`. ----
    /// The current download folder, as a path. Updated by `set_download_dir`
    /// alongside `download_fs`, since `download_dir_config` is only ever the
    /// value `new` was given. `Engine::auto_copy` reads this to report
    /// `AutoCopy.destination`.
    pub(crate) download_dir: Mutex<PathBuf>,
    /// Every device's stored automatic copy setting.
    pub(crate) auto_copy: Mutex<crate::auto_copy::AutoCopyStore>,
    /// Every file this device has pulled to completion, manual or automatic.
    pub(crate) held: Mutex<crate::held::HeldStore>,
    /// Which devices have an automatic copy run in flight right now. One run
    /// per device at a time.
    pub(crate) auto_copy_running: Mutex<std::collections::HashSet<String>>,
    // ---- Item 18: trusted networks. See `networks.rs`. ----
    /// What [`apply_presence`] last decided about
    /// [`networks::browse_allowed`], which does not need `reachable`.
    ///
    /// The `Browser` is created and owned by [`browse_loop`]'s own thread,
    /// so nothing else can drop it. This flag is how that loop sees the
    /// rule: it reads it on every pass, and drops its `Browser` while the
    /// flag is false, because a browse query is a sound on the network.
    pub(crate) browsing: AtomicBool,
    /// Held by whichever thread is applying the presence rule.
    ///
    /// [`apply_presence`] takes this before it reads the state, and holds it
    /// across the advertiser write and the `browsing` write. Without it two
    /// threads can read the inputs in one order and write the advertiser in
    /// the other, which leaves the advertiser off while the rule says on, or
    /// on while the rule says off, and nothing reapplies the rule until the
    /// next input changes. No caller of [`apply_presence`] holds the state
    /// lock when it calls, so taking this first and the state lock second is
    /// the one order every thread uses.
    pub(crate) presence: Mutex<()>,
}

impl Shared {
    /// True once `stop` has begun.
    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Wait up to `how_long`, or until `stop` wakes every thread.
    ///
    /// Returns false when the engine is stopping, so a loop can end.
    ///
    /// `stop` notifies `wake` without holding this lock, so a thread that
    /// reaches this call after that notify would wait out the whole of
    /// `how_long` with nobody left to wake it, while `stop` joins it. The
    /// pairing watchdog passes two minutes, so `stop` would take that long.
    /// `stop` also sets `state.stopped` under this same lock, before it
    /// notifies. Reading that flag here, while the lock is held and before
    /// the wait begins, closes the window: `stop` cannot have passed its
    /// own first block without this seeing the flag set there.
    pub(crate) fn rest(&self, how_long: Duration) -> bool {
        let guard = lock(&self.state);
        if guard.stopped {
            return false;
        }
        let (guard, _) = self
            .wake
            .wait_timeout(guard, how_long)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(guard);
        !self.stopping()
    }

    /// Keep a thread so `stop` can join it, and drop handles that already
    /// finished.
    ///
    /// G7: `stop` sets `stopping` and then takes every handle `joins` holds
    /// under this same lock, once. A handle whose thread was spawned just
    /// before that but reaches this call just after it would otherwise be
    /// added to an empty `joins` nobody will ever look at again, so `stop`
    /// returns without ever waiting for it. Refusing it here instead, once
    /// `stopping` is already set, closes that race: the thread is left to
    /// notice `stopping` and end on its own, the same as any other thread
    /// `stop` cannot join (`lib.rs`, "a serving thread cannot be woken").
    pub(crate) fn keep(&self, handle: JoinHandle<()>) {
        let mut joins = lock(&self.joins);
        if self.stopping() {
            return;
        }
        joins.retain(|h| !h.is_finished());
        joins.push(handle);
    }

    /// This device's connection pool, made on first use.
    ///
    /// `docs/engine-contract.md`, item 19. Every caller gets the same pool
    /// for the same key, so four connections is the count across the whole
    /// engine, not per bridge.
    ///
    /// No pool is made for a device that is not paired. The pools lock is
    /// held across that check, and `Engine::forget` holds the same lock
    /// across its own removal and the peer list write, so a call that
    /// passed the check before `forget` began cannot make the pool again
    /// after `forget` dropped it.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when the key hex does not decode or
    /// names no stored device.
    pub(crate) fn pool_for(&self, device_key_hex: &str) -> Result<Arc<Pool>, FerryError> {
        let key = key_from_hex(device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        let mut pools = lock(&self.pools);
        if lock(&self.state).peers.get(&key).is_none() {
            return Err(failed("Runtime::NotPaired"));
        }
        Ok(Arc::clone(
            pools
                .entry(device_key_hex.to_owned())
                .or_insert_with(|| Arc::new(Pool::new(device_key_hex.to_owned()))),
        ))
    }

    /// The download folder, if `start` has opened it.
    pub(crate) fn download_fs(&self) -> Option<Arc<LocalFs>> {
        lock(&self.download_fs).clone()
    }

    /// Where one transfer's record is stored.
    pub(crate) fn record_path(&self, id: &str) -> PathBuf {
        self.transfers_dir.join(format!("{id}.bin"))
    }

    /// Where one batch's record is stored.
    pub(crate) fn batch_path(&self, id: &str) -> PathBuf {
        self.batches_dir.join(id)
    }

    /// A connection id no other open connection is using right now.
    pub(crate) fn next_connection_id(&self) -> u64 {
        self.next_connection.fetch_add(1, Ordering::SeqCst)
    }

    /// Register a connection's raw socket under `id`, so `stop` can close it
    /// directly. `docs/engine-contract.md` item 16c.
    pub(crate) fn register_socket(&self, id: u64, socket: TcpStream) {
        lock(&self.sockets).insert(id, socket);
    }

    /// Remove a connection's registered socket. Called once the connection
    /// has ended, whether it finished, failed, or was closed by `stop`.
    pub(crate) fn unregister_socket(&self, id: u64) {
        lock(&self.sockets).remove(&id);
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
                state.pairing.offer_nonce = None;
                state.pairing.requested = None;
            }
        }
        // docs/engine-contract.md item 18: pairing in progress is one of the
        // three things that turn Wi-Fi presence on, and this is the one
        // place every pairing state is written, so every pairing start and
        // every pairing end reaches the rule from here.
        apply_presence(self);
        self.notify.pairing(next);
    }
}

/// The claim on one data folder.
///
/// Two engines on one folder each keep the whole paired device list in
/// memory and each write the whole file, so the second one to write puts
/// back what the first one removed. A forgotten device would come back.
/// `data_dir/lock` is what stops that, but the lock is the kernel's, held on
/// this open file, not the file's existence: a second engine, in this
/// process or another, that tries to lock the same file while this one is
/// held is refused before it opens anything.
///
/// The kernel drops the lock the instant this file descriptor closes, for
/// any reason: `stop`, a normal process exit, or a kill signal that gives
/// the process no chance to run its own cleanup. A file a killed process
/// left behind therefore holds no lock and never blocks the next
/// `Engine::new`. It still carries that process's identifier, for a person
/// who wants to know who last used the folder; nothing in Ferry reads that
/// back.
pub(crate) struct DirLock {
    file: std::fs::File,
    held: AtomicBool,
}

impl DirLock {
    /// Claim the folder with an OS advisory lock, or report that somebody
    /// else holds it.
    fn take(path: &std::path::Path) -> Result<Self, FerryError> {
        // Not `truncate(true)`: truncating happens at open, before the lock
        // is decided, and would blank a live holder's file out from under
        // it. Truncation happens below, only once this call has the lock.
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| bad_config("The lock file could not be opened."))?;
        match file.try_lock() {
            Ok(()) => {
                // Clear whatever an earlier holder left, then write this
                // process's identifier for a person to read. Nothing
                // depends on either step: the lock itself is the kernel's,
                // not this text.
                drop(file.set_len(0));
                drop(std::io::Write::write_all(
                    &mut file,
                    format!("{}\n", std::process::id()).as_bytes(),
                ));
                Ok(Self {
                    file,
                    held: AtomicBool::new(true),
                })
            }
            Err(std::fs::TryLockError::WouldBlock) => Err(bad_config(
                "Another copy of Ferry is running. Quit it and try again.",
            )),
            Err(std::fs::TryLockError::Error(_)) => {
                Err(bad_config("The lock file could not be locked."))
            }
        }
    }

    /// Give the folder back. Doing this twice is safe.
    pub(crate) fn release(&self) {
        if self.held.swap(false, Ordering::SeqCst) {
            // An unlock that fails leaves nothing worse than closing the
            // file does on its own: the kernel drops the lock either way.
            drop(self.file.unlock());
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

/// Record one finished operation on the calling side, as actor `This`, and
/// finalise it at once.
///
/// Unlike a served connection, which stays open across many operations, a
/// calling-side operation such as `list` or one transfer attempt owns its
/// connection for exactly one round of work. So every call here gets a
/// fresh connection id and ends it in the same breath, the way
/// `pull_folder` finalises its own listing (docs/engine-contract.md, item
/// 13, "Rolling up").
///
/// `pub(crate)` rather than private: `dav::server` calls this too, for a
/// bridge request's own `PROPFIND` and `GET` (item 6, S3).
pub(crate) fn record_this(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    verb: access::AccessVerb,
    path: &str,
    bytes: Option<u64>,
    entries: Option<u32>,
    files: Option<u32>,
) {
    let connection = shared.next_connection_id();
    let now = now_unix_secs();
    let mut log = lock(&shared.access_log);
    let Some(rollup) = log.as_mut() else {
        return;
    };
    rollup.touch(
        now,
        connection,
        EntryFields {
            device_key_hex: device_key_hex.to_owned(),
            actor: access::Actor::This,
            verb,
            path: path.to_owned(),
            bytes,
            entries,
            files,
        },
    );
    rollup.connection_ended(now, connection);
}

/// The checks every item 19 call makes before it touches the wire.
///
/// `docs/engine-contract.md`, item 19. Parses the path, then proves the
/// engine has started and the device is paired, and hands back the parsed
/// path with the device's pool for the caller to borrow from. Every one of
/// `list`, `stat`, `read_at`, `write_at`, `truncate`, `mkdir`, `delete`
/// and `rename` opens with this, so they refuse the same things in the
/// same order.
///
/// # Errors
///
/// Returns a `PathError` code when the path is refused,
/// `Runtime::NotStarted` before `Engine::start` has run, and
/// `Runtime::NotPaired` when the key hex does not decode or names no
/// stored device. The paired check and the pool are taken together under
/// the pools lock, so a device forgotten in between is never dialed.
fn remote_call(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    remote_path: &str,
) -> Result<(RemotePath, Arc<Pool>), FerryError> {
    let path = RemotePath::parse(remote_path).map_err(from_path)?;
    if key_from_hex(device_key_hex).is_none() {
        return Err(failed("Runtime::NotPaired"));
    }
    if !lock(&shared.state).started {
        return Err(failed("Runtime::NotStarted"));
    }
    // The paired check and the pool both live in `pool_for`, under one hold
    // of the pools lock. Checking here and making the pool afterwards let a
    // call that passed the check before `forget` wrote the peer list make a
    // fresh pool for the device it had just forgotten, and dial it.
    let pool = shared.pool_for(device_key_hex)?;
    Ok((path, pool))
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
    change: impl FnOnce(&mut PeerStore) -> Result<(), PeerError>,
) -> Result<(), FerryError> {
    let writing = lock(&shared.peers_write);
    let mut copy = lock(&shared.state).peers.clone();
    let result = change(&mut copy).and_then(|()| copy.save());
    if result.is_ok() {
        lock(&shared.state).peers = copy;
    }
    drop(writing);
    result.map_err(|e| from_peer(&e))
}

/// Change the trusted Wi-Fi network list and write it out.
///
/// The same shape as [`save_peers`], for the same reason: writing the list
/// calls `fsync`, so the state lock is not held across it. The list is
/// cloned, changed and written under its own writer lock, and the state lock
/// is taken only to swap the result in.
///
/// # Errors
///
/// Whatever `change` returns. Nothing is published in that case, so memory
/// and disk still agree.
fn save_networks(
    shared: &Arc<Shared>,
    change: impl FnOnce(&mut crate::networks::TrustedNetworks) -> Result<bool, FerryError>,
) -> Result<bool, FerryError> {
    let writing = lock(&shared.networks_write);
    let mut copy = lock(&shared.state).trusted.clone();
    let result = change(&mut copy);
    if matches!(result, Ok(true)) {
        lock(&shared.state).trusted = copy;
    }
    drop(writing);
    result
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

/// Build and validate the roots a `Config` or `set_roots` call names.
///
/// # Errors
///
/// Returns a `RootsError` code, forwarded from [`Roots::open`], for
/// anything wrong with `roots` itself: no roots, an invalid or duplicate
/// name, a path that is not an existing folder, or two roots that overlap.
/// An empty `roots` from `Config` is caught before this runs, and reported
/// as `Runtime::BadConfig` instead; see [`Engine::new`].
fn open_roots(roots: &[Root]) -> Result<RootsState, FerryError> {
    let specs: Vec<RootSpec> = roots.iter().cloned().map(RootSpec::from).collect();
    let opened = Roots::open(specs).map_err(from_roots)?;
    Ok(RootsState {
        specs: roots.to_vec(),
        opened: Arc::new(opened),
    })
}

/// The last component of a path, for display.
///
/// `pub(crate)`: `push.rs` reuses this for a push's own `file_name`.
pub(crate) fn leaf_of(path: &RemotePath) -> String {
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

/// Delete one batch's record, and say so when it cannot be done.
///
/// # Errors
///
/// Returns `TransferError::Local` when the file is there and will not go.
fn remove_batch(shared: &Arc<Shared>, id: &str) -> Result<(), FerryError> {
    match std::fs::remove_file(shared.batch_path(id)) {
        Ok(()) => Ok(()),
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
    let (source, destination, bytes_total, bytes_done, source_size, source_mtime, meta, chunk_size) =
        match record {
            Record::FirstPass(meta, pass) => (
                pass.source.clone(),
                pass.destination.clone(),
                pass.source_size,
                pass.bytes_done(),
                Some(pass.source_size),
                Some(pass.source_mtime),
                meta,
                // `pass.chunk_size` was a valid `ChunkSize` when this record
                // was written, since nothing else ever produces one. The
                // fallback here is never reached in practice; it exists so
                // a damaged record cannot panic this load.
                ChunkSize::new(pass.chunk_size).unwrap_or_else(|_| ChunkSize::one_mebibyte()),
            ),
            // A ready record's bytes are counted again by the resume itself,
            // which reads the partial file back and hashes it. Its manifest
            // carries the exact chunk size it was built with: `set_chunk_size`
            // after this transfer's first pass ran must not change what
            // `chunks_total` reports for it, so this comes from the record,
            // never from the engine's current setting.
            Record::Ready(meta, transfer) => (
                transfer.source.clone(),
                transfer.destination.clone(),
                transfer.manifest.length(),
                0,
                None,
                None,
                meta,
                transfer.manifest.chunk_size(),
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
        batch_id: meta.batch_id.clone(),
        chunk_size,
    }
}

/// Read every batch record left by an earlier run.
///
/// Runs after `load_saved_transfers`, so a batch's transfer ids can be
/// checked against what actually survived. A batch none of whose transfer
/// ids name a surviving row is dropped, and its file removed: the same rule
/// a single finished transfer already follows on its own, since its record
/// is deleted the moment it becomes `Done`. A batch file that does not
/// decode at all is removed the same way, rather than left on disk for
/// every future start to skip again. See `load_saved_transfers`.
fn load_saved_batches(shared: &Arc<Shared>) {
    let Ok(entries) = std::fs::read_dir(&shared.batches_dir) else {
        return;
    };
    let mut to_remove: Vec<PathBuf> = Vec::new();
    {
        let mut state = lock(&shared.state);
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            let Some(key_hex) = id.split_once('-').map(|(key, _)| key.to_owned()) else {
                continue;
            };
            let Some(record) = batch::read_batch(&path) else {
                to_remove.push(path);
                continue;
            };
            let survives = record
                .transfer_ids
                .iter()
                .any(|transfer_id| state.transfers.contains_key(transfer_id));
            if survives {
                state.batches.insert(
                    id.clone(),
                    BatchRow {
                        id,
                        device_key_hex: key_hex,
                        label: record.label,
                        direction: record.direction,
                        origin: record.origin,
                        started_unix_secs: record.started_unix_secs,
                        transfer_ids: record.transfer_ids,
                        done_files: record.done_files,
                        done_bytes: record.done_bytes,
                    },
                );
            } else {
                to_remove.push(path);
            }
        }
    }
    for path in to_remove {
        drop(std::fs::remove_file(path));
    }
}

/// Compare the browse-allowed and Wi-Fi presence rules to what is running,
/// and start or stop the advertiser and the browser to match.
///
/// `docs/engine-contract.md`, item 18. This is the only place the advertiser
/// is started, and the only place that decides whether the browse loop holds
/// a `Browser`. It gates the browser by [`networks::browse_allowed`], which
/// does not need `reachable`, and the advertiser by
/// [`networks::wifi_presence`], which does. Every site that changes an input
/// calls it: `set_reachable`, `set_network`, `trust_network`,
/// `forget_network`, `Shared::set_pairing`, which covers every pairing start
/// and every pairing end, `start`, and `stop`.
///
/// An advertiser that is already running is left alone. Restarting one gives
/// it a fresh random instance name, and [`Engine::short_code`] shows the last
/// four characters of that name next to this device in the other side's
/// pairing list, so a restart during pairing would change the code a person
/// is reading off two screens.
///
/// A stopped engine is left alone too. `stop` turns the advertiser and the
/// browse flag off itself and this does nothing from then on, so a
/// `set_reachable(true)` that arrives after `stop` cannot announce a port
/// that is already closed. `stop` sets `stopped` before it takes the
/// presence lock, so a thread already inside this one either read `stopped`
/// as false and finished before `stop`'s own writes, or reads it as true and
/// returns.
pub(crate) fn apply_presence(shared: &Shared) {
    // One rule, applied by one thread at a time. Held across the state read
    // and both writes below, so two threads cannot read the inputs in one
    // order and write the advertiser in the other.
    let _presence = lock(&shared.presence);
    let (browsing, present, port) = {
        let state = lock(&shared.state);
        if state.stopped {
            return;
        }
        (
            networks::browse_allowed(&state),
            networks::wifi_presence(&state),
            state.listen_addr.map(|addr| addr.port()),
        )
    };
    shared.browsing.store(browsing, Ordering::SeqCst);
    {
        let mut advertiser = lock(&shared.advertiser);
        if present {
            if advertiser.is_none()
                && let Some(port) = port
            {
                // A network that refuses multicast still allows the cable and
                // a known address, so this failure does not stop the switch.
                *advertiser = Advertiser::start(port).ok();
            }
        } else {
            *advertiser = None;
        }
    }
    // The browse loop rests between passes, so this is what makes it read
    // the flag now rather than at the end of its current wait.
    shared.wake.notify_all();
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
        let remote = pending.remote();
        let welcome = {
            let state = lock(&shared.state);
            welcomes_inbound(
                state.reachable,
                networks::wifi_presence(&state),
                state.pairing.accepts_inbound(),
                remote,
            )
        };
        if !welcome {
            // This is what `set_reachable(false)`, and an untrusted network,
            // mean. The connection is dropped after accept, not before,
            // because a listener cannot refuse before accepting.
            drop(pending);
            continue;
        }
        shared.accepted.fetch_add(1, Ordering::SeqCst);
        let shared = Arc::clone(shared);
        // A serving thread is not joined. See the crate documentation.
        drop(std::thread::spawn(move || handle_inbound(&shared, pending)));
    }
}

/// Decide what one accepted connection is, and run it.
///
/// `docs/engine-contract.md` item 12: the pre-handshake exchange now carries
/// a mode byte, so this reads it once, with `Pending::negotiate`, before it
/// picks a Noise pattern, rather than guessing purely from local state.
/// Local state still gates each mode: a `PairByCode` or `PairByQr` request
/// is only honoured while this device is actually open to that one method,
/// and a mismatched request is simply dropped, the same way a `Connect`
/// request from a stranger with no matching key already was.
fn handle_inbound(shared: &Arc<Shared>, pending: Pending) {
    let remote = pending.remote();
    let Ok(negotiated) = pending.negotiate() else {
        // A version or a handshake timeout. Nothing to report: a peer that
        // cannot even negotiate learns nothing more by being told so.
        return;
    };
    match negotiated.mode() {
        Mode::PairByCode => {
            if lock(&shared.state).pairing.is_open_to_pairing() {
                accept_pairing(shared, negotiated, remote);
            }
        }
        Mode::PairByQr => {
            if lock(&shared.state).pairing.is_offering() {
                accept_qr_offer(shared, negotiated, remote);
            }
        }
        Mode::Connect => {
            // With no stored peer there is nobody this connection could be, and
            // `candidate_peers` still returns one candidate nobody holds, so a
            // device with no peer and a device with one peer look the same from
            // outside. Dropping the connection here instead would tell a stranger
            // which of the two this device is.
            let candidates = candidate_peers(shared, remote);
            // A refused handshake is the design working: whoever called does not
            // hold a key this device paired with. Nothing to report.
            if let Ok(connection) = negotiated.connect(&shared.key, &candidates) {
                let peer = connection.peer;
                serve_connection(shared, connection, peer);
            }
        }
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

/// Run the code pairing handshake as the side that accepted the connection.
fn accept_pairing(shared: &Arc<Shared>, negotiated: NegotiatedPending, remote: SocketAddr) {
    match negotiated.pair(&shared.key) {
        // A refused hold means another pairing is already showing its code.
        // The person is looking at that one, so nothing is reported here.
        Ok(connection) => drop(hold_pairing(shared, connection, remote, true)),
        Err(error) => report_pairing_failure(shared, &error),
    }
}

/// Run the QR pairing handshake as the side whose key was in the offer.
fn accept_qr_offer(shared: &Arc<Shared>, negotiated: NegotiatedPending, remote: SocketAddr) {
    let expected_nonce = lock(&shared.state).pairing.offer_nonce;
    match negotiated.pair_ik(&shared.key, expected_nonce.as_ref()) {
        // A refused hold means another scan is already `Requested`. Nothing
        // to report; see `hold_qr_pairing`.
        Ok(connection) => drop(hold_qr_pairing(shared, connection, remote)),
        // A wrong or guessed nonce is a stranger, not a failure of this
        // offer: the connection is simply dropped, and the offer stays
        // live for the real phone to still scan and complete. Every other
        // handshake failure still ends the offer, the same as any other
        // pairing failure does.
        Err(TcpError::Noise(NoiseError::UnknownOffer)) => {}
        Err(error) => report_pairing_failure(shared, &error),
    }
}

/// Report a failed handshake, unless pairing has already moved on.
///
/// Someone who cancelled while the handshake ran must not see a failure for
/// a pairing they already stopped.
fn report_pairing_failure(shared: &Arc<Shared>, error: &TcpError) {
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

/// Show `Requested` and hold the connection until someone confirms.
///
/// `NegotiatedPending::pair_ik` already checked the handshake's nonce
/// against `expected_nonce` before this runs, so reaching this function at
/// all means some handshake matched the offer. What is still checked here,
/// under the same lock that spends the nonce, is whether another scan
/// already won that race: the same check `hold_pairing` makes for the code
/// method, guarding the same kind of race.
///
/// # Errors
///
/// Returns `Runtime::PairingBusy` when something is already `Requested`, or
/// when pairing has moved on.
fn hold_qr_pairing(
    shared: &Arc<Shared>,
    connection: IkPairedConnection,
    addr: SocketAddr,
) -> Result<(), FerryError> {
    let IkPairedConnection { accepted, .. } = connection;
    let name = accepted.name.clone();
    let kind = accepted.kind;
    {
        let mut state = lock(&shared.state);
        if !state.pairing.is_offering() {
            return Err(failed("Runtime::PairingBusy"));
        }
        // The nonce is single use. Spending it here, on the winning side of
        // the race above, is what makes a second scan of the same code
        // refused instead of merely unlucky.
        state.pairing.offer_nonce = None;
        state.pairing.requested = Some(RequestedPairing {
            stream: accepted.stream,
            peer: accepted.peer,
            addr,
            name: accepted.name,
            kind,
            hello_done: false,
        });
    }
    shared.set_pairing(&PairingState::Requested {
        name,
        kind: kind.into(),
        transport: Transport::Wifi,
    });
    Ok(())
}

/// Show `Requested` on the side that scanned, and hold the session open.
///
/// The counterpart to [`hold_qr_pairing`], on the other end of the same
/// handshake. The scan proved the static key came from a screen. It did not
/// show the person whose screen, and a person who scanned the wrong code
/// has no way to tell from the handshake alone. The names cross in
/// `exchange_hello`, so this runs after that exchange and publishes the
/// name it carried. `confirm_pairing` then stores the peer, or drops it.
///
/// The session waits in `requested` meanwhile, unread, the same way the
/// offering side's does. Nothing reads it, so the stopping wrapper
/// `hello_with_deadline` put around it has no more work to do and comes off
/// here.
///
/// Does nothing when pairing has already moved on, which is what the
/// watchdog does once the two minute deadline passes.
fn hold_scanned_pairing(
    shared: &Arc<Shared>,
    peer_key: PublicKey,
    stream: StopAware<SecureStream>,
    addr: SocketAddr,
    name: String,
    kind: CoreDeviceKind,
) {
    {
        let mut state = lock(&shared.state);
        if state.pairing.requested.is_some() || !state.pairing.is_running() {
            return;
        }
        state.pairing.requested = Some(RequestedPairing {
            stream: stream.into_inner(),
            peer: peer_key,
            addr,
            name: name.clone(),
            kind,
            hello_done: true,
        });
    }
    shared.set_pairing(&PairingState::Requested {
        name,
        kind: kind.into(),
        transport: Transport::Wifi,
    });
}

/// What `confirm_pairing` took out of the pairing state.
enum Confirming {
    /// A code method connection. The names have not crossed yet.
    Code(HeldPairing),
    /// A QR method connection, on either side of the scan.
    Scan(RequestedPairing),
}

/// Carry out an accepted confirm on whichever connection was held.
fn pair_after_confirm(shared: &Arc<Shared>, taken: Confirming) {
    match taken {
        Confirming::Code(held) => finish_pairing(
            shared,
            held.connection.paired.peer,
            held.connection.paired.stream,
            held.addr,
            held.accepted,
            None,
        ),
        Confirming::Scan(RequestedPairing {
            stream,
            peer,
            addr,
            name,
            kind,
            hello_done,
        }) => {
            if hello_done {
                // The scanning side. The names crossed before `Requested`
                // was shown, so there is nothing left to ask. This side
                // dialed, so it does not serve on this stream either, and
                // letting it go is all that is left to do with it.
                drop(stream);
                store_paired_peer(shared, peer, addr, &name, kind);
            } else {
                // The offering side. Message one carried the other device's
                // hello, and the exchange `finish_pairing` runs must agree
                // with it.
                finish_pairing(shared, peer, stream, addr, true, Some(&(name, kind)));
            }
        }
    }
}

/// Report a failed pairing, unless pairing has already moved on.
fn fail_pairing(shared: &Arc<Shared>, error: FerryError) {
    if !lock(&shared.state).pairing.is_running() {
        return;
    }
    shared.set_pairing(&PairingState::Failed { error });
}

/// Add the network this device is on now to the trusted list.
///
/// `docs/engine-contract.md`, item 18: the first pairing at home trusts
/// home. Called from [`finish_pairing`], which is the one place both pairing
/// methods store a peer, so this covers both methods and both sides.
///
/// An unknown network adds nothing. A full list, or a name this build would
/// refuse, adds nothing either: pairing succeeded, and a list that cannot
/// grow is not a reason to fail it. `Shared::set_pairing` reapplies the rule
/// right after this, when it reports `Confirmed`.
fn trust_current_network(shared: &Arc<Shared>) {
    let Some(name) = lock(&shared.state).network.clone() else {
        return;
    };
    drop(save_networks(shared, |list| list.add(&name)));
}

/// Store the peer, trust the network, and report the new device.
///
/// The second half of every pairing: both methods, both sides. Each side
/// reaches it only after its own person confirmed, so a confirm on one
/// device never stores anything on the other.
///
/// Returns true when it reported `Confirmed`. False means pairing had
/// already ended, or storing failed, and the caller must go no further.
fn store_paired_peer(
    shared: &Arc<Shared>,
    peer_key: PublicKey,
    addr: SocketAddr,
    name: &str,
    kind: CoreDeviceKind,
) -> bool {
    // The watchdog may have given up while the names crossed, or while the
    // person was reading the name. A pairing that already reported Failed
    // must not store a device or report Confirmed after it.
    if !lock(&shared.state).pairing.is_running() {
        return false;
    }

    let key_hex = hex_of(&peer_key);
    if let Err(error) = save_peers(shared, |store| {
        store.add(Peer {
            key: peer_key,
            name: name.to_owned(),
            paired_unix_secs: now_unix_secs(),
            kind,
        })
    }) {
        fail_pairing(shared, error);
        return false;
    }
    trust_current_network(shared);

    let device = {
        let mut state = lock(&shared.state);
        let live = state.live_mut(&key_hex);
        live.last_addr = Some(addr);
        live.last_seen_unix_secs = Some(now_unix_secs());
        state.device(&key_hex)
    };

    let Some(device) = device else {
        fail_pairing(shared, failed("Runtime::NotPaired"));
        return false;
    };
    if !lock(&shared.state).pairing.is_running() {
        return false;
    }
    shared.set_pairing(&PairingState::Confirmed { device });
    notify(shared, Change::Devices);
    true
}

/// Exchange names, store the peer, and report the new device.
///
/// Every path that still has a hello to run: the code method on both sides,
/// and the offering side of a scan. The scanning side ran its hello before
/// it asked its own person, so `pair_after_confirm` calls
/// [`store_paired_peer`] directly for it instead.
///
/// `expected_hello` is the scan's message-one hello, from
/// `RequestedPairing`, when the offering side is what `confirm_pairing`
/// took; `None` for the code method, which has no earlier hello to check
/// against. When it is `Some`, the hello this function exchanges here must
/// agree with it, or the pairing fails: the name and kind shown in
/// `Requested`, that a person already confirmed against, must be the same
/// identity this finishes pairing with.
fn finish_pairing(
    shared: &Arc<Shared>,
    peer_key: PublicKey,
    stream: SecureStream,
    addr: SocketAddr,
    accepted: bool,
    expected_hello: Option<&(String, CoreDeviceKind)>,
) {
    let (name, kind, stream) = match hello_with_deadline(shared, stream) {
        Ok(triple) => triple,
        Err(error) => {
            fail_pairing(shared, error);
            return;
        }
    };
    if let Some((expected_name, expected_kind)) = expected_hello
        && (name != *expected_name || kind != *expected_kind)
    {
        // The scan's message one and this later hello disagree about who
        // this is: treated as an ordinary bad hello, the same code
        // `exchange_hello` itself returns when the first frame is not a
        // hello at all.
        fail_pairing(shared, failed("RpcError::UnexpectedFrameKind"));
        return;
    }

    if !store_paired_peer(shared, peer_key, addr, &name, kind) {
        return;
    }
    let key_hex = hex_of(&peer_key);

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
            addr,
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
) -> Result<(String, CoreDeviceKind, StopAware<SecureStream>), FerryError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let my_name = shared.display_name.clone();
    let my_kind = shared.kind;
    let stopping = Arc::clone(&shared.stopping);
    // Not joined. It ends when the exchange ends, or when the wrapper below
    // fails the next read because the engine is stopping.
    drop(std::thread::spawn(move || {
        let mut stream = StopAware::new(stream, stopping);
        let outcome =
            exchange_hello(&mut stream, &my_name, my_kind).map(|(name, kind)| (name, kind, stream));
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
        state.pairing.offer_nonce = None;
        state.pairing.requested = None;
        running
    };
    if running {
        shared.set_pairing(&PairingState::Failed {
            error: failed("Runtime::PairingTimeout"),
        });
    }
}

/// Atomically checks that no pairing is running and, if not, claims the
/// slot: resets pairing to idle, arms the shared two minute deadline, and
/// starts the watchdog that gives up once it passes. Returns the deadline
/// in Unix seconds, for the caller's own first published state.
///
/// The check and the reset happen under one lock on `shared.state`, held
/// the whole time. `start_pairing_with` and `offer_scanned` used to check
/// `is_running` and then call this as two separate lock acquisitions, which
/// let a second call land in between and start a second pairing attempt of
/// its own; going through this one function closes that gap for both.
///
/// Shared by both of `start_pairing_with`'s branches and by
/// `offer_scanned`'s dial, so "two minutes, one watchdog" stays one fact
/// instead of three copies of it. `docs/engine-contract.md` item 12.
///
/// # Errors
///
/// Returns `None`, with nothing reset, while a pairing is already running.
fn begin_pairing_deadline(shared: &Arc<Shared>) -> Option<i64> {
    let timeout = *lock(&shared.pairing_timeout);
    let expires_unix_secs = now_unix_secs() + i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX);
    {
        let mut state = lock(&shared.state);
        if state.pairing.is_running() {
            return None;
        }
        state.pairing = Pairing::idle();
        state.pairing.deadline = Some(Instant::now() + timeout);
        state.pairing.deadline_unix_secs = Some(expires_unix_secs);
    }
    let watchdog_shared = Arc::clone(shared);
    shared.keep(std::thread::spawn(move || {
        pairing_watchdog(&watchdog_shared);
    }));
    Some(expires_unix_secs)
}

/// This device's non-loopback interface addresses, each paired with `port`.
///
/// Built for a QR pairing offer: the offering device has to state where it
/// can be dialed, and unlike the phone (`ferry_core::discovery::Advertiser`)
/// it never advertises over mDNS, so there is no existing list of its own
/// addresses to read back. `if-addrs` enumerates the system's network
/// interfaces directly instead.
///
/// This build does not try to tell a Wi-Fi interface apart from any other
/// kind by name or platform API; a Mac used to show a pairing QR code has
/// one non-loopback, non-link-local interface worth offering in the
/// ordinary case, and a finer distinction is future work.
///
/// Two kinds of address are excluded outright, not merely deprioritised.
/// Loopback, since neither Wi-Fi nor the cable is ever a loopback address,
/// and a phone could not dial one anyway. Link-local (`169.254.0.0/16` and
/// `fe80::/10`), since a link-local address is only meaningful together
/// with the interface it came from, and the offer's wire format
/// (`ferry_core::offer`) has no field for that interface index: an
/// unqualified link-local address is not merely low priority, it is
/// ambiguous, and a real machine hands back several of them at once, on
/// tunnel and peer-to-peer interfaces nobody is dialing over. Trying to
/// connect to one anyway does not fail fast; the OS holds the attempt open,
/// so a handful of them ahead of the one real address in the list can cost
/// most of a minute before `dial_offer` ever reaches it.
///
/// A machine with no usable interface gets an offer with zero addresses;
/// the phone that scans it fails to dial any and reports
/// `Runtime::NotReachable`, the same as it would for a paired device that
/// dropped off the network.
///
/// Capped at [`ferry_core::offer::MAX_DIAL_ADDRESSES`], private-range
/// addresses first, by [`ferry_core::offer::dialable_addresses`]: the same
/// policy `dial_offer` applies to a scanned offer's own list, since a
/// machine with many interfaces should not draw a QR code that makes a
/// phone try dialing all of them.
fn local_wifi_addresses(port: u16) -> Vec<SocketAddr> {
    let addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| SocketAddr::new(interface.ip(), port));
    ferry_core::offer::dialable_addresses(addresses)
}

/// Make a QR offer and publish `Offering`, or `Failed` if the system has no
/// randomness to make a nonce with.
///
/// `expires_unix_secs` is the deadline `start_pairing_with` already
/// claimed through `begin_pairing_deadline`, atomically with its running
/// check: by the time this runs, pairing is already reset to idle and the
/// watchdog is already racing against that deadline. A failure here still
/// reports `Failed` correctly: `set_pairing` clears the deadline the
/// watchdog is waiting on once pairing is no longer running.
fn start_offering(shared: &Arc<Shared>, expires_unix_secs: i64) {
    let mut nonce = [0u8; QR_NONCE_LEN];
    if getrandom::fill(&mut nonce).is_err() {
        shared.set_pairing(&PairingState::Failed {
            error: failed("Runtime::NoRandomness"),
        });
        return;
    }

    let port = lock(&shared.net)
        .as_ref()
        .map_or(shared.listen_port, |net| net.local_addr().port());
    let offer = Offer {
        version: 1,
        static_key: shared.key.public(),
        expires_unix_secs,
        nonce,
        addresses: local_wifi_addresses(port),
    };
    lock(&shared.state).pairing.offer_nonce = Some(nonce);
    shared.set_pairing(&PairingState::Offering {
        offer: PairingOffer {
            payload: offer.encode(),
            expires_unix_secs,
        },
    });
}

/// Dial a scanned offer's addresses in order, run `IK` as the initiator,
/// exchange names, and then ask this side's own person about the name that
/// came back, through [`hold_scanned_pairing`]. `docs/engine-contract.md`
/// item 12: both sides confirm by name, and each stores only after its own
/// confirm.
///
/// `offer.addresses` came from a scanned QR code, so it is not trusted as
/// bounded or ordered: it is filtered the same way `local_wifi_addresses`
/// filters this device's own, through
/// [`ferry_core::offer::dialable_addresses`], before a single address is
/// dialed. A real offer's addresses always survive that filter already,
/// since `local_wifi_addresses` is what produced them; reaching an offer
/// whose every address is loopback or link-local means something unusual,
/// not a hostile flood, since the cap below still bounds that case the
/// same as any other, so the un-filtered list is tried instead of dialing
/// nothing. The loop also gives up as soon as `stop` begins, so a `stop`
/// that lands while this is dialing an unreachable address does not wait
/// for every remaining one first.
fn dial_offer(shared: &Arc<Shared>, offer: &Offer) {
    let filtered = ferry_core::offer::dialable_addresses(offer.addresses.iter().copied());
    let addresses = if filtered.is_empty() {
        offer
            .addresses
            .iter()
            .copied()
            .take(ferry_core::offer::MAX_DIAL_ADDRESSES)
            .collect()
    } else {
        filtered
    };
    let mut last_error = None;
    for addr in &addresses {
        if shared.stopping() {
            return;
        }
        match tcp::pair_ik(
            *addr,
            &shared.key,
            &offer.static_key,
            &offer.nonce,
            &shared.display_name,
            shared.kind,
        ) {
            Ok(stream) => {
                // Names cross, and then this side asks its own person about
                // the name it got; see `hold_scanned_pairing`.
                let (name, kind, stream) = match hello_with_deadline(shared, stream) {
                    Ok(triple) => triple,
                    Err(error) => {
                        fail_pairing(shared, error);
                        return;
                    }
                };
                hold_scanned_pairing(shared, offer.static_key, stream, *addr, name, kind);
                return;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if shared.stopping() {
        return;
    }
    let error = last_error.map_or_else(|| failed("Runtime::NotReachable"), |e| from_tcp(&e));
    fail_pairing(shared, error);
}

/// The stored peers to offer `Pending::connect` as candidates, in the order
/// a responder should try them: the one whose last known address matches
/// `remote` first, then the rest.
///
/// The wire says nothing about who is calling before the handshake, so every
/// stored peer is a candidate; `Pending::connect` reads message one once and
/// tries each in turn (`docs/engine-contract.md` item 16b). Storage already
/// refuses more than `peers::MAX_PEERS` (64) peers, which already bounds how
/// many are ever tried here.
///
/// A device with no stored peer gets exactly one candidate, a key nobody
/// holds, so a stranger sees the same shape of failure whether or not this
/// device has ever paired.
fn candidate_peers(shared: &Arc<Shared>, remote: SocketAddr) -> Vec<PublicKey> {
    let state = lock(&shared.state);
    let mut peers = state.peers.all();
    if peers.is_empty() {
        return vec![nobody(shared)];
    }
    peers.sort_by_key(|peer| {
        let matches_address = state
            .live
            .get(&hex_of(&peer.key))
            .and_then(|live| live.last_addr)
            .is_some_and(|addr| addr.ip() == remote.ip());
        // `false` sorts before `true`, so a matching address comes first.
        !matches_address
    });
    peers.into_iter().map(|peer| peer.key).collect()
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
    let socket_id = shared.next_connection_id();
    let _socket = SocketRegistration::new(shared, socket_id, connection.socket);
    serve_stream(
        shared,
        connection.stream,
        peer,
        transport,
        connection.remote,
        &allowed,
    );
}

/// Keeps one connection's raw socket registered in `Shared`, for `stop` to
/// close directly, for as long as this value lives.
///
/// `docs/engine-contract.md` item 16c: every connection registers when it is
/// established and removes itself when it ends. Using a guard, instead of a
/// bare register-then-unregister pair, means an early return or a panic on
/// any path still frees the entry.
pub(crate) struct SocketRegistration<'a> {
    shared: &'a Arc<Shared>,
    id: u64,
}

impl<'a> SocketRegistration<'a> {
    pub(crate) fn new(shared: &'a Arc<Shared>, id: u64, socket: TcpStream) -> Self {
        shared.register_socket(id, socket);
        Self { shared, id }
    }
}

impl Drop for SocketRegistration<'_> {
    fn drop(&mut self) {
        self.shared.unregister_socket(self.id);
    }
}

/// Serve the shared root on one stream, and keep the device list honest.
fn serve_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    addr: SocketAddr,
    allowed: &Arc<AtomicBool>,
) {
    let Ok((name, _kind)) = exchange_hello(&mut stream, &shared.display_name, shared.kind) else {
        release_serving(shared, &hex_of(&peer), allowed);
        return;
    };
    serve_named_stream(shared, stream, peer, transport, addr, name, allowed);
}

/// Serve the shared root on a stream whose names were already exchanged.
fn serve_named_stream(
    shared: &Arc<Shared>,
    mut stream: impl std::io::Read + std::io::Write,
    peer: PublicKey,
    transport: Transport,
    addr: SocketAddr,
    name: String,
    allowed: &Arc<AtomicBool>,
) {
    let key_hex = hex_of(&peer);
    let renamed = {
        let state = lock(&shared.state);
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
    // docs/engine-contract.md, batch B, item 3: the dialling side already
    // calls `mark_reachable` before it serves a connection; the accepting
    // side never did, so it never recorded a Wi-Fi success and
    // `available_transports` never listed `Wifi` for the caller once the
    // connection ended. Calling the same path here records it on both
    // transports, the same way the dialling side does.
    mark_reachable(shared, &key_hex, addr, transport);
    if let Some(peer) = renamed {
        // Outside the lock. Writing the list calls `fsync`, and any peer can
        // ask for this by sending a name of its own choosing.
        drop(save_peers(shared, |store| store.add(peer)));
    }
    notify(shared, Change::Devices);

    if lock(&shared.roots).is_none() {
        release_serving(shared, &key_hex, allowed);
        return;
    }
    // `GuardedFs` holds this handle, not a snapshot, so a `set_roots` call
    // reaches this connection on its very next operation.
    let connection = shared.next_connection_id();
    let guarded = GuardedFs::new(
        Arc::clone(&shared.roots),
        Arc::clone(allowed),
        Arc::clone(&shared.access_log),
        connection,
        key_hex.clone(),
    );
    // A connection that ends is the ordinary outcome. The error, if any, has
    // nowhere useful to go: the person did not ask for this connection.
    drop(serve(&mut stream, &guarded));

    // docs/engine-contract.md, item 13: a served connection ending is what
    // finalises whatever it was still in the middle of.
    if let Some(rollup) = lock(&shared.access_log).as_mut() {
        rollup.connection_ended(now_unix_secs(), connection);
    }

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

/// Watch mDNS for the whole session, while browsing is allowed.
///
/// `docs/engine-contract.md`, item 18. The thread runs for the whole session
/// as it always did, but the `Browser` only exists while `Shared::browsing`
/// is true, because a browse query is a sound on the network and a device in
/// a café makes none. This does not need `reachable`: it is what lets a Mac
/// with its presence switch off still find and mount a phone. A `Browser`
/// this loop drops shuts its own mDNS daemon down, so nothing is left
/// querying.
fn browse_loop(shared: &Arc<Shared>) {
    let mut browser: Option<Browser> = None;
    while !shared.stopping() {
        if !shared.browsing.load(Ordering::SeqCst) {
            browser = None;
            if !shared.rest(BROWSE_TICK) {
                break;
            }
            continue;
        }
        if browser.is_none() {
            let Ok(started) = Browser::start() else {
                // A network that refuses multicast leaves the cable and a
                // known address, both of which work without this loop. Trying
                // again on the next pass costs one daemon start per
                // `BROWSE_TICK`, which is what a healthy loop spends on one
                // `next` call anyway, and it is what lets a browser appear
                // when browsing becomes allowed again on a network that does
                // allow multicast.
                if !shared.rest(BROWSE_TICK) {
                    break;
                }
                continue;
            };
            browser = Some(started);
        }
        if let Some(found) = browser.as_ref() {
            match found.next(BROWSE_TICK) {
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

/// Finalise idle access log entries and prune old day files, for the whole
/// session `start` runs.
///
/// One loop does both jobs, so pruning needs no periodic thread of its
/// own: it already has to wake every [`ACCESS_LOG_TICK`] to give the
/// roll-up's five second idle rule somewhere to run, which is the same
/// cadence `notify.rs` reports on, and an hourly job can ride along on top
/// of that (docs/engine-contract.md, item 13).
fn access_log_loop(shared: &Arc<Shared>) {
    let mut next_prune = Instant::now() + ACCESS_LOG_PRUNE;
    loop {
        let changed = {
            let mut log = lock(&shared.access_log);
            log.as_mut().is_some_and(|rollup| {
                rollup.tick(now_unix_secs());
                rollup.take_changed()
            })
        };
        if changed {
            notify(shared, Change::AccessLog);
        }
        if Instant::now() >= next_prune {
            if let Some(rollup) = lock(&shared.access_log).as_mut() {
                drop(rollup.prune(now_unix_secs()));
            }
            next_prune = Instant::now() + ACCESS_LOG_PRUNE;
        }
        if !shared.rest(ACCESS_LOG_TICK) {
            return;
        }
    }
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
    let gone: Vec<&UsbForward> = known
        .iter()
        .filter(|f| !serials.contains(&f.serial))
        .collect();
    for forward in &gone {
        drop(adb.remove_forward(&forward.serial, forward.local_port));
    }
    {
        let mut state = lock(&shared.state);
        state.forwards.clone_from(&current);
        // A device's `usb_port` names the forward it was last reached
        // through. Once that forward is gone, the cable is gone too, and
        // `available_transports` must drop `Usb` for it rather than keep
        // showing a port nothing answers on any more.
        let gone_ports: Vec<u16> = gone.iter().map(|f| f.local_port).collect();
        clear_gone_usb_forwards(&mut state.live, &gone_ports);
    }

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
    let became_reachable = {
        let mut state = lock(&shared.state);
        let live: &mut DeviceLive = state.live_mut(key_hex);
        let was_reachable = live.reachable_via.is_some();
        live.reachable_via = Some(via);
        live.last_addr = Some(addr);
        live.last_seen_unix_secs = Some(now_unix_secs());
        if via == Transport::Usb {
            live.usb_port = Some(addr.port());
        } else {
            live.last_wifi_success_unix_secs = Some(now_unix_secs());
        }
        !was_reachable
    };
    if became_reachable {
        // docs/engine-contract.md item 14: the first of the run's three
        // triggers. Outside the lock just released, since this may spawn a
        // thread.
        crate::auto_copy::on_became_reachable(shared, key_hex);
    }
}
