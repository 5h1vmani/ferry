//! The `WebDAV` bridge: turns one paired device's shared roots into a
//! loopback HTTP server Finder can mount.
//!
//! `docs/engine-contract.md`, item 6, and ADR 0008. I1: browsing.
//! `OPTIONS`, `PROPFIND` at depth 0 and 1, `GET`, `HEAD`, `LOCK`, `UNLOCK`,
//! and a `PUT` of a sidecar name. I2: `PUT` of a real file, `MKCOL`,
//! `DELETE`, `MOVE`, `COPY`, `PROPPATCH`, and the `If` header's lock check
//! on all of them.
//!
//! One [`MountRegistry`] lives in [`crate::engine::Shared`], one entry per
//! device that has ever had [`MountRegistry::start`] called for it. Each
//! entry owns a plain loopback [`std::net::TcpListener`] (never the
//! Noise-encrypted transport the rest of the engine speaks to peers on),
//! one accept thread, and one thread per connection, matching every other
//! transport in this engine.
//!
//! # Files
//!
//! - `http.rs`: parses a request line, headers, and a content-length body;
//!   writes a status line and headers; formats the two date shapes the
//!   protocol needs.
//! - `xml.rs`: reads a `PROPFIND` request body for the properties it asks
//!   for, tolerantly, and writes the `multistatus` reply.
//! - `probes.rs`: the fixed list of Apple metadata names, and the sidecar
//!   store under `data_dir/dav_sidecars/`.
//! - `lock.rs`: the lock table `LOCK` and `UNLOCK` share, with tokens and a
//!   timeout.
//! - `cache.rs`: the two second depth 1 listing cache.
//! - `heads.rs`: item 17's head cache, and the queue the prefetch thread
//!   reads listings from.
//! - The connection pool the bridge borrows from lives in `crate::pool`,
//!   shared with `list` and the remote operations of item 19.
//! - `put.rs`: item I2's landing rule for `PUT` of a real file and for
//!   `COPY`, reusing item 5's push rule.
//! - `delete.rs`: item I2's recursive `DELETE` plan, over `folder.rs`'s
//!   bounds.
//! - `server.rs`: the accept loop, the connection loop, and the reply
//!   helpers every verb writes through.
//! - `handlers/`: one file per verb group. `browse.rs` for `PROPFIND`
//!   and `PROPPATCH`, `read.rs` for `GET` and `HEAD`, `write.rs` for
//!   `PUT`, `MKCOL`, `DELETE`, `MOVE` and `COPY`, `locks.rs` for `LOCK`
//!   and `UNLOCK`, and `options.rs` for `OPTIONS` and the auth check.
//! - `prefetch.rs`: the thread that reads the head of each listed file.
//! - `errors.rs`: turns an RPC failure into a status line.

pub(crate) mod cache;
mod delete;
mod errors;
mod handlers;
mod heads;
mod http;
mod lock;
mod prefetch;
mod probes;
mod put;
mod server;
mod xml;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::errors::{failed, failed_with};
use crate::pool::Pool;
use crate::state::lock as lock_mutex;
use crate::{FerryError, MountEndpoint};

/// Sixteen random bytes, thirty-two lowercase hex characters. Used for the
/// per-start mount password and for each lock token.
///
/// # Errors
///
/// Returns `Runtime::MountFailed` when the platform's randomness source is
/// exhausted, the same fate every other `getrandom::fill` call in this
/// engine has.
pub(crate) fn random_hex(n_bytes: usize) -> Result<String, FerryError> {
    let mut bytes = vec![0u8; n_bytes];
    getrandom::fill(&mut bytes)
        .map_err(|_| failed_with("Runtime::MountFailed", "no randomness was available"))?;
    let mut out = String::with_capacity(n_bytes * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// One running bridge: the endpoint it answers as, and the handles to stop
/// it.
struct Mount {
    endpoint: MountEndpoint,
    port: u16,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Item 17's prefetch thread, and the bridge whose queue wakes it.
    /// Both are needed to end it, so `stop` holds both.
    bridge: Arc<server::Bridge>,
    prefetch: Option<JoinHandle<()>>,
    /// The same pool [`server::Bridge::new`] was given, below in
    /// [`MountRegistry::start`], kept here too so `stop` can shut its open
    /// connections down without reaching through `Bridge`'s own private
    /// field. `docs/audits/fable-lifecycle.md`, finding 2.
    pool: Arc<Pool>,
}

/// One [`MountRegistry::start`] in flight for one device key.
///
/// `docs/audits/fable-lifecycle.md`, finding 8: the first caller for a key
/// builds this and does the real work; every other concurrent caller for
/// the same key finds it already in [`MountRegistry::starting`] and waits
/// on it instead, so a second concurrent `start` never sweeps the spool,
/// binds a second port, or spawns a second pair of threads for one device.
struct InProgress {
    result: Mutex<Option<Result<MountEndpoint, FerryError>>>,
    done: Condvar,
}

impl InProgress {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            done: Condvar::new(),
        }
    }

    /// Blocks until [`InProgress::finish`] has run, then returns a copy of
    /// what it recorded.
    fn wait(&self) -> Result<MountEndpoint, FerryError> {
        let mut result = lock_mutex(&self.result);
        while result.is_none() {
            result = self
                .done
                .wait(result)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        result
            .clone()
            .unwrap_or_else(|| unreachable!("the loop above only exits once this is Some"))
    }

    /// Records the leader's result and wakes every waiter.
    fn finish(&self, result: Result<MountEndpoint, FerryError>) {
        *lock_mutex(&self.result) = Some(result);
        self.done.notify_all();
    }
}

/// Every device's `WebDAV` bridge, by device key hex.
///
/// One instance lives in [`crate::engine::Shared`] for the life of the
/// engine. Starting a bridge that is already running returns its existing
/// endpoint rather than opening a second port, so `Engine::mount_start` is
/// idempotent.
pub(crate) struct MountRegistry {
    mounts: Mutex<BTreeMap<String, Mount>>,
    /// One entry per device key with a [`MountRegistry::start`] in flight
    /// right now. `docs/audits/fable-lifecycle.md`, finding 8.
    starting: Mutex<BTreeMap<String, Arc<InProgress>>>,
    /// The spool folder's running byte total, across every device.
    /// `docs/audits/fable-engineering.md`, finding 4: kept exact instead of
    /// walked on every `PUT` and `COPY`. See [`put::SpoolBytes`].
    spool_bytes: put::SpoolBytes,
}

impl MountRegistry {
    pub(crate) fn new() -> Self {
        Self {
            mounts: Mutex::new(BTreeMap::new()),
            starting: Mutex::new(BTreeMap::new()),
            spool_bytes: put::SpoolBytes::new(),
        }
    }

    /// The spool folder's total size right now. Test only, through
    /// `Engine::spool_bytes`: proves the running count kept by
    /// [`put::SpoolBytes`] matches what a walk of the folder would find,
    /// without paying for that walk. Not exported to the apps.
    pub(crate) fn spool_bytes_total(&self) -> u64 {
        self.spool_bytes.get()
    }

    /// Start serving `device_key_hex`'s roots over `WebDAV` on a random
    /// loopback port. Idempotent: a device that already has a bridge keeps
    /// it, and this returns that bridge's own endpoint unchanged.
    ///
    /// The caller (`Engine::mount_start`) has already checked that the
    /// device is paired, and passes `device_name` for the mount root's
    /// `displayname` (N4): the peer's own stored name, not anything a DAV
    /// request could influence.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::MountFailed` when the loopback port cannot be
    /// bound or the password cannot be generated, `Runtime::NotPaired`
    /// when the device was forgotten between the caller's own check and
    /// this call, and `Runtime::NotReachable` when the engine is stopping.
    pub(crate) fn start(
        &self,
        shared: &Arc<crate::engine::Shared>,
        device_key_hex: &str,
        device_name: &str,
    ) -> Result<MountEndpoint, FerryError> {
        // `docs/audits/fable-lifecycle.md`, finding 8: this used to hold
        // `self.mounts` across the spool sweep, the bind, and both spawns
        // below, so one device's `start` blocked `stop` and `stop_all` for
        // any other device for as long as that took. The lock is now taken
        // twice instead: once here, to return an existing bridge's endpoint
        // at once and to leave a per key mark for a concurrent second call
        // to find, and once more in `start_uncontested`, to publish the
        // result. A second concurrent call for the same key waits on that
        // mark rather than repeating the sweep, the bind, and the spawns.
        let in_progress = {
            if let Some(mount) = lock_mutex(&self.mounts).get(device_key_hex) {
                return Ok(mount.endpoint.clone());
            }
            let mut starting = lock_mutex(&self.starting);
            if let Some(mount) = lock_mutex(&self.mounts).get(device_key_hex) {
                // Lost the race to insert a mark below to a call that
                // already finished and inserted into `self.mounts` in the
                // gap between the two checks above.
                return Ok(mount.endpoint.clone());
            }
            if let Some(existing) = starting.get(device_key_hex) {
                Err(Arc::clone(existing))
            } else {
                let mine = Arc::new(InProgress::new());
                starting.insert(device_key_hex.to_owned(), Arc::clone(&mine));
                Ok(mine)
            }
        };
        let mine = match in_progress {
            Ok(mine) => mine,
            // A `start` for this key is already in flight. Its own result,
            // once it has one, is this call's result too: `Ok` with the one
            // bridge it built, or whatever error it hit.
            Err(waiting_on) => return waiting_on.wait(),
        };

        let result = self.start_uncontested(shared, device_key_hex, device_name);
        lock_mutex(&self.starting).remove(device_key_hex);
        mine.finish(result.clone());
        result
    }

    /// The real work of [`MountRegistry::start`], run by whichever caller's
    /// [`InProgress`] mark won the race in [`MountRegistry::starting`].
    fn start_uncontested(
        &self,
        shared: &Arc<crate::engine::Shared>,
        device_key_hex: &str,
        device_name: &str,
    ) -> Result<MountEndpoint, FerryError> {
        // `Engine::stop` sets `stopping` before it ever touches
        // `self.mounts`. Checked again here, right before the slow work
        // below begins, refuses a `start` that reaches this point after a
        // `stop` already under way; the second check at the insert, below,
        // is what actually closes the race, for a `stop` that runs its
        // whole course while this call is still doing that work.
        if shared.stopping() {
            return Err(failed("Runtime::NotReachable"));
        }

        // A fresh start only: a bridge already running for this device may
        // hold live spool files of its own, which this must never touch.
        // Anything still here is left over from a crash or an ungraceful
        // quit before this device's bridge last stopped.
        put::sweep_spool(shared, device_key_hex);

        // Finding 4: the first bridge this engine ever starts walks the
        // whole spool folder once, to seed the running count every later
        // `PUT` and `COPY` keeps exact. Every start after that is a no-op
        // here; `self.spool_bytes` already tracks reality.
        self.spool_bytes
            .init_from(&shared.data_dir.join("dav_spool"));

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| failed_with("Runtime::MountFailed", &error.to_string()))?;
        let port = listener
            .local_addr()
            .map_err(|error| failed_with("Runtime::MountFailed", &error.to_string()))?
            .port();
        let password = random_hex(16)?;
        let user = "ferry".to_owned();
        let endpoint = MountEndpoint {
            url: format!("http://127.0.0.1:{port}/"),
            user: user.clone(),
            password: password.clone(),
        };

        // Item 19: the engine's pool for this device, not one of the
        // bridge's own, so `Engine::list` and this bridge share it. A
        // device that is not paired has no pool, so no bridge is built for
        // one. Kept here, not only inside `Bridge`, so `stop` can reach it;
        // see `Mount::pool`.
        let pool = shared.pool_for(device_key_hex)?;
        let running = Arc::new(AtomicBool::new(true));
        let bridge = Arc::new(server::Bridge::new(
            device_key_hex.to_owned(),
            device_name.to_owned(),
            user,
            password,
            port,
            Arc::clone(&pool),
            shared,
        ));

        let shared_for_thread = Arc::clone(shared);
        let bridge_for_thread = Arc::clone(&bridge);
        let running_for_thread = Arc::clone(&running);
        let handle = std::thread::spawn(move || {
            let listener = listener;
            server::accept_loop(
                &shared_for_thread,
                &bridge_for_thread,
                &listener,
                &running_for_thread,
            );
        });

        // Item 17: one prefetch thread per bridge, ended by the same
        // `running` flag as the accept thread and joined the same way.
        let shared_for_prefetch = Arc::clone(shared);
        let bridge_for_prefetch = Arc::clone(&bridge);
        let running_for_prefetch = Arc::clone(&running);
        let prefetch = std::thread::spawn(move || {
            prefetch::prefetch_loop(
                &shared_for_prefetch,
                &bridge_for_prefetch,
                &running_for_prefetch,
            );
        });

        let mut mounts = lock_mutex(&self.mounts);
        // Re-checked under the same lock `stop_all`'s own copy of the keys
        // takes: `docs/audits/fable-lifecycle.md`, finding 8, and the
        // comment above this function. If `Engine::stop` ran its whole
        // course, unseen, while this call did the slow work above, its
        // `stop_all` copied the keys `self.mounts` held before this insert,
        // so this bridge would never be joined, and would outlive `stop`.
        // Tearing down what was just built, instead of publishing it, is
        // what the original single, whole-call lock hold did for free.
        if shared.stopping() {
            drop(mounts);
            running.store(false, Ordering::SeqCst);
            drop(std::net::TcpStream::connect(("127.0.0.1", port)));
            bridge.stop_prefetch();
            drop(handle.join());
            drop(prefetch.join());
            return Err(failed("Runtime::NotReachable"));
        }
        mounts.insert(
            device_key_hex.to_owned(),
            Mount {
                endpoint: endpoint.clone(),
                port,
                running,
                handle: Some(handle),
                bridge,
                prefetch: Some(prefetch),
                pool,
            },
        );
        Ok(endpoint)
    }

    /// Stop serving `device_key_hex`, and close its port. Safe to call on a
    /// device with no running bridge.
    ///
    /// Blocks until the accept thread has ended, so the port is free the
    /// moment this returns. A connection already being served is not
    /// joined, the same known limitation `crate::lib` documents for every
    /// other transport in this engine: it ends when the peer goes away.
    pub(crate) fn stop(&self, device_key_hex: &str) {
        let mount = lock_mutex(&self.mounts).remove(device_key_hex);
        let Some(mut mount) = mount else {
            return;
        };
        mount.running.store(false, Ordering::SeqCst);
        // The accept loop is blocked inside `accept`. A connection to our
        // own port is the only way to bring it back, the same trick
        // `engine::wake_the_listener` uses for the peer-facing listener.
        drop(std::net::TcpStream::connect(("127.0.0.1", mount.port)));
        // `docs/audits/fable-lifecycle.md`, finding 2: the prefetch thread
        // below may be inside a blocked read of a peer that stopped
        // answering, or inside its own wait for a free connection, either
        // of which could otherwise hold this call, and so `Engine::forget`
        // and `Engine::mount_stop` on the app's main thread, for as long as
        // the wire's own idle timeout. `pause` first, so a waiter the
        // shutdown below wakes does not dial straight back into the peer
        // this call is stopping; `shutdown_open` then ends the read and the
        // wait at once, the same way `Engine::stop` ends one on every
        // socket it has registered. `unpause`, once the join below has
        // returned, is what leaves `Engine::list` and a later `mount_start`
        // unaffected: this device may still be paired.
        mount.pool.pause();
        mount.pool.shutdown_open();
        // The prefetch thread is either waiting for a listing or between
        // two files. Ending its queue covers the first case; the `running`
        // flag above covers the second (item 17).
        mount.bridge.stop_prefetch();
        if let Some(handle) = mount.handle.take() {
            drop(handle.join());
        }
        if let Some(prefetch) = mount.prefetch.take() {
            drop(prefetch.join());
        }
        mount.pool.unpause();
    }

    /// Stop every bridge. Called by `Engine::stop`.
    pub(crate) fn stop_all(&self) {
        let keys: Vec<String> = lock_mutex(&self.mounts).keys().cloned().collect();
        for key in keys {
            self.stop(&key);
        }
    }
}
