//! The state every thread shares, and the notifications it sends.
//!
//! `Shared` is the one object each spawned thread holds an `Arc` of.
//! `DirLock` is the OS lock that keeps a second engine out of a data
//! directory. The saving helpers write the files that outlive a run.

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ferry_core::adb::Adb;
use ferry_core::chunk::ChunkSize;
use ferry_core::discovery::Advertiser;
use ferry_core::localfs::LocalFs;
use ferry_core::noise::StaticKey;
use ferry_core::peers::{DeviceKind as CoreDeviceKind, PeerError, PeerStore};
use ferry_core::tcp::Listener;

use crate::access::{self, EntryFields};
use crate::dav;
use crate::errors::{bad_config, failed, from_peer};
use crate::guard::{AccessLogHandle, RootsHandle};
use crate::notify::{Change, Notify};
use crate::pool::Pool;
use crate::state::{State, key_from_hex, lock, now_unix_secs};
use crate::{FerryError, PairingState, Root};

use crate::networks::apply_presence;

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
    /// How many inbound connections are alive right now: accepted, and
    /// still being served by their own thread, regardless of the mode that
    /// connection agreed to or which peer, if any, it authenticates as.
    ///
    /// Unlike `accepted`, this counts down too, as each thread ends.
    /// `docs/audits/fable-engineering.md`, finding 2: `accept_loop`
    /// refuses to spawn a thread once this reaches
    /// [`MAX_INBOUND_CONNECTIONS`], the same way `ferry_core::tcp` refuses
    /// a pending handshake past its own cap.
    pub(crate) inbound: AtomicU32,
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
    pub(crate) fn take(path: &std::path::Path) -> Result<Self, FerryError> {
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
        match try_lock_exclusive(&file) {
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
            Err(LockFailure::Held) => Err(bad_config(
                "Another copy of Ferry is running. Quit it and try again.",
            )),
            Err(LockFailure::Other) => Err(bad_config("The lock file could not be locked.")),
        }
    }

    /// Give the folder back. Doing this twice is safe.
    pub(crate) fn release(&self) {
        if self.held.swap(false, Ordering::SeqCst) {
            // An unlock that fails leaves nothing worse than closing the
            // file does on its own: the kernel drops the lock either way.
            drop(unlock(&self.file));
        }
    }
}

/// Why an exclusive lock was not taken.
enum LockFailure {
    /// Another process holds it.
    Held,
    /// The file system or the platform refused the lock itself.
    Other,
}

/// Take an exclusive, non-blocking advisory lock on `file`.
///
/// This calls `flock` through `rustix` instead of
/// `std::fs::File::try_lock`. The standard library's lock is not supported
/// on the Android target and reports an error there for every file, which
/// made every `Engine::new` on the phone fail on 11 September 2026.
/// `flock` is one system call on macOS, Linux, and Android, and the kernel
/// releases it when the process ends, however it ends.
#[cfg(unix)]
fn try_lock_exclusive(file: &std::fs::File) -> Result<(), LockFailure> {
    match rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::WOULDBLOCK) => Err(LockFailure::Held),
        Err(_) => Err(LockFailure::Other),
    }
}

#[cfg(unix)]
fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock).map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn try_lock_exclusive(file: &std::fs::File) -> Result<(), LockFailure> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockFailure::Held),
        Err(std::fs::TryLockError::Error(_)) => Err(LockFailure::Other),
    }
}

#[cfg(not(unix))]
fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    file.unlock()
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
pub(crate) fn save_networks(
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
