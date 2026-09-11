//! A small pool of the engine's own [`Client`] connections to one device,
//! four at most, taken and returned per request.
//!
//! `docs/engine-contract.md`, item 6: "so one stalled Finder request does
//! not hold every other one." A request that needs the peer borrows a
//! connection, uses it, and gives it back; a fifth concurrent request waits
//! a bounded time for one to free up rather than opening a fifth socket.
//!
//! Item 19 makes this the engine's one pool per device, not the bridge's.
//! [`crate::engine::Shared`] owns one [`Pool`] per device key, made on
//! first use and dropped by `forget` and by `stop`. The `WebDAV` bridge and
//! [`crate::Engine::list`] both borrow from that one pool, so a Finder
//! request and a file picker listing share the same four connections
//! instead of opening a fifth socket each time.
//!
//! # Reachability, and the 503 rule
//!
//! Item 6 also asks that "when the device is not reachable, every request
//! answers 503 at once." A fresh dial through `transfer::dial` can still
//! take real time against a stale address, because the underlying
//! `TcpStream::connect` in `ferry-core::tcp` carries no connect timeout of
//! its own; that is an existing, separate limitation this module does not
//! reach into `tcp.rs` to fix. Instead, before dialing at all, [`Pool::take`]
//! reads the device's own last known reachability from `Shared::state`, the
//! same fact `DeviceInfo.reachable_via` reports. When that is `None`, this
//! returns `Runtime::NotReachable` immediately, with no dial attempted,
//! which covers the ordinary case the spec is written for: a phone that is
//! off or out of range. A device whose address has gone stale since its
//! last successful connection, while still marked reachable, is not
//! covered; that gap belongs to `transfer::dial` itself and is shared by
//! every other caller of it.
//!
//! That rule is written for Finder, so only the bridge takes it.
//! [`Pool::take_dialing`], which every engine call in item 19 uses, dials a
//! device that is not marked reachable instead of refusing it. An app call
//! is what learns a device is reachable in the first place.

use std::collections::HashSet;
use std::net::Shutdown;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ferry_core::noise::SecureStream;
use ferry_core::rpc::{Client, RpcError, exchange_hello};

use crate::FerryError;
use crate::engine::{Shared, mark_reachable};
use crate::errors::{failed, from_rpc};
use crate::guard::StopAware;
use crate::state::{key_from_hex, lock as lock_mutex};

/// How many connections to one device this bridge holds open at once,
/// idle and borrowed together.
const MAX_CONNECTIONS: usize = 4;

/// How long a fifth request waits for one of the four to free up before it
/// gives up and answers 503 itself. Thirty seconds: long enough that a
/// normal folder open over Wi-Fi, queued behind four others already busy
/// with the peer, does not 503 the person just because Finder asked at a
/// bad moment.
const WAIT_FOR_SLOT: Duration = Duration::from_secs(30);

type PeerClient = Client<StopAware<SecureStream>>;

/// One pooled connection and the id its raw socket is registered under, so
/// `stop` can close it (`docs/engine-contract.md` item 16c). Dropping it
/// removes the registration.
struct Pooled {
    client: PeerClient,
    id: u64,
    shared: Arc<Shared>,
}

impl Drop for Pooled {
    fn drop(&mut self) {
        self.shared.unregister_socket(self.id);
    }
}

struct Inner {
    idle: Vec<Pooled>,
    outstanding: usize,
    /// The id of every connection currently borrowed out, so
    /// [`Pool::shutdown_open`] can find their sockets in `Shared::sockets`
    /// without a `Pooled` of its own to read the id from. An idle
    /// connection's id needs no separate bookkeeping: it is read straight
    /// off the `Pooled` sitting in `idle`.
    outstanding_ids: HashSet<u64>,
    /// True once [`Pool::close`] has run. A closed pool never dials again
    /// and never hands out an idle connection again.
    closed: bool,
    /// True for exactly as long as [`Pool::pause`] and [`Pool::unpause`]
    /// bracket a call: `MountRegistry::stop` holds this across its own
    /// join. `docs/audits/fable-lifecycle.md`, finding 2: without it, a
    /// waiter [`Pool::shutdown_open`] just woke by shutting down the
    /// connection someone else was holding would dial straight back into
    /// the peer this call is trying to stop talking to. Unlike `closed`,
    /// this is lifted again once the call it guards returns, so
    /// `Engine::list` and a later `mount_start` are unaffected once the
    /// mount has actually stopped.
    paused: bool,
    /// The engine this pool dials through, learned from its first dial and
    /// used only by [`Pool::shutdown_open`] to reach `Shared::sockets`.
    /// Every dial this pool ever makes is for the one engine that built it,
    /// so the first one learned is the only one there ever is.
    shared_for_shutdown: Option<Arc<Shared>>,
}

pub(crate) struct Pool {
    device_key_hex: String,
    inner: Mutex<Inner>,
    slot_freed: Condvar,
}

impl Pool {
    pub(crate) fn new(device_key_hex: String) -> Self {
        Self {
            device_key_hex,
            inner: Mutex::new(Inner {
                idle: Vec::new(),
                outstanding: 0,
                outstanding_ids: HashSet::new(),
                closed: false,
                paused: false,
                shared_for_shutdown: None,
            }),
            slot_freed: Condvar::new(),
        }
    }

    /// Borrows a connection for the `WebDAV` bridge: an idle one if there
    /// is one, a freshly dialed one if the pool has room, or a wait of up
    /// to [`WAIT_FOR_SLOT`] for either.
    ///
    /// A device that is not currently known to be reachable is refused at
    /// once, with no dial attempted. That is item 6's rule: "While the
    /// phone is not reachable, the bridge answers 503 at once rather than
    /// hanging, so Finder shows an error instead of a beachball." See the
    /// module documentation for why the check reads stored reachability
    /// rather than timing the dial out.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when the device's key hex cannot be
    /// decoded, `Runtime::NotReachable` when the device is not currently
    /// known to be reachable, when the wait for a free slot times out, or
    /// when the dial itself fails, and forwards `exchange_hello`'s own
    /// error otherwise.
    pub(crate) fn take(&self, shared: &Arc<Shared>) -> Result<Borrowed<'_>, FerryError> {
        self.take_inner(shared, true)
    }

    /// Borrows a connection for an engine call the app made itself, such as
    /// [`crate::Engine::list`] or [`crate::Engine::stat`].
    ///
    /// The same pool and the same four connections as [`Pool::take`], with
    /// one difference: a device that is not currently marked reachable is
    /// dialed rather than refused. `docs/engine-contract.md`, item 19.
    /// Item 6's answer-at-once rule is written for Finder, which must never
    /// beachball; an app call is the very thing that learns a device is
    /// reachable, because [`Pool::dial`] calls `mark_reachable` on success.
    /// Refusing it here would mean no engine call could ever reach a device
    /// that had not already been reached.
    ///
    /// # Errors
    ///
    /// As [`Pool::take`], except that it never returns
    /// `Runtime::NotReachable` without attempting a dial.
    pub(crate) fn take_dialing(&self, shared: &Arc<Shared>) -> Result<Borrowed<'_>, FerryError> {
        self.take_inner(shared, false)
    }

    /// The body both entry points share. `require_reachable` is the one
    /// difference between them.
    fn take_inner(
        &self,
        shared: &Arc<Shared>,
        require_reachable: bool,
    ) -> Result<Borrowed<'_>, FerryError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Learned once, from whichever call dials first. `shutdown_open`
        // reads it back to find this pool's own sockets in
        // `Shared::sockets`; every other call after the first is a no-op.
        inner
            .shared_for_shutdown
            .get_or_insert_with(|| Arc::clone(shared));
        loop {
            // `docs/engine-contract.md`, item 19: a device this engine has
            // forgotten has no connection left to borrow, and no dial to
            // make. A bridge connection Finder still holds is refused here.
            //
            // `docs/audits/fable-lifecycle.md`, finding 2: `paused` is the
            // same refusal, held only for as long as `MountRegistry::stop`
            // is tearing this device's bridge down, so a waiter
            // `Pool::shutdown_open` just woke does not dial straight back
            // into the peer that call is trying to stop talking to.
            if inner.closed || inner.paused {
                return Err(failed("Runtime::NotReachable"));
            }
            if let Some(client) = inner.idle.pop() {
                inner.outstanding += 1;
                inner.outstanding_ids.insert(client.id);
                return Ok(Borrowed::new(self, client));
            }
            if inner.idle.len() + inner.outstanding < MAX_CONNECTIONS {
                inner.outstanding += 1;
                drop(inner);
                return match self.dial(shared, require_reachable) {
                    Ok(client) => {
                        self.inner
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .outstanding_ids
                            .insert(client.id);
                        Ok(Borrowed::new(self, client))
                    }
                    Err(error) => {
                        self.give_back(None, false);
                        Err(error)
                    }
                };
            }
            let (next, timeout) = self
                .slot_freed
                .wait_timeout(inner, WAIT_FOR_SLOT)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if timeout.timed_out() {
                return Err(failed("Runtime::NotReachable"));
            }
            // A woken, not timed out, wait loops back to the top, where
            // `closed` and `paused` are checked before anything else: this
            // is what stops a waiter `Pool::shutdown_open` just woke, by
            // shutting down the connection someone else was holding, from
            // dialing straight back into a peer this call is stopping.
            inner = next;
        }
    }

    /// Dials the device fresh. See the module documentation for why this
    /// checks known reachability first rather than always dialing, and
    /// [`Pool::take_dialing`] for the one caller that skips that check.
    fn dial(&self, shared: &Arc<Shared>, require_reachable: bool) -> Result<Pooled, FerryError> {
        let key = key_from_hex(&self.device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        if require_reachable {
            let reachable = lock_mutex(&shared.state)
                .live
                .get(&self.device_key_hex)
                .is_some_and(|live| live.reachable_via.is_some());
            if !reachable {
                return Err(failed("Runtime::NotReachable"));
            }
        }
        let (stream, socket, addr, via) =
            crate::transfer::dial(shared, &self.device_key_hex, &key)?;
        mark_reachable(shared, &self.device_key_hex, addr, via);
        let id = shared.next_connection_id();
        shared.register_socket(id, socket);
        let mut stream = StopAware::new(stream, Arc::clone(&shared.stopping));
        if let Err(error) = exchange_hello(&mut stream, &shared.display_name, shared.kind) {
            shared.unregister_socket(id);
            return Err(from_rpc(&error));
        }
        Ok(Pooled {
            client: Client::new(stream),
            id,
            shared: Arc::clone(shared),
        })
    }

    /// Takes back a borrowed connection. `client` is `None` when the dial
    /// that would have produced one failed instead; `healthy` is
    /// meaningless in that case. A connection found broken, or given back
    /// to a closed pool, is dropped rather than reused.
    fn give_back(&self, client: Option<Pooled>, healthy: bool) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.outstanding = inner.outstanding.saturating_sub(1);
        let mut to_drop = None;
        if let Some(client) = client {
            inner.outstanding_ids.remove(&client.id);
            if healthy && !inner.closed {
                inner.idle.push(client);
            } else {
                // Dropped outside the lock below: `Pooled::drop` unregisters
                // the socket, which takes `Shared::sockets`, a different
                // lock than this one.
                to_drop = Some(client);
            }
        }
        drop(inner);
        drop(to_drop);
        self.slot_freed.notify_one();
    }

    /// Shut every idle connection down and refuse every later borrow.
    ///
    /// `docs/engine-contract.md`, item 19. `Engine::forget` calls this. The
    /// registry's own handle on the pool goes with the same call, but a
    /// bridge connection Finder already holds keeps its own `Arc<Bridge>`,
    /// which keeps this `Arc<Pool>`. Without this, that connection's next
    /// request popped an idle connection nobody had closed and read a
    /// forgotten device's files. After this, [`Pool::take`] and
    /// [`Pool::take_dialing`] both answer `Runtime::NotReachable`.
    ///
    /// A connection borrowed right now is not interrupted; the call it is
    /// running finishes and [`Pool::give_back`] then drops it.
    pub(crate) fn close(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.closed = true;
        let idle = std::mem::take(&mut inner.idle);
        drop(inner);
        // A shutdown turns a peer's blocked read into an error at once,
        // rather than waiting for the idle timeout in `tcp.rs`. Dropping
        // each `Pooled` below closes its own copy of the socket and
        // unregisters it. `docs/engine-contract.md` item 16c.
        for pooled in &idle {
            if let Some(socket) = lock_mutex(&pooled.shared.sockets).get(&pooled.id) {
                drop(socket.shutdown(Shutdown::Both));
            }
        }
        drop(idle);
        self.slot_freed.notify_all();
    }

    /// Refuses every `take` and `take_dialing` until [`Pool::unpause`] lifts
    /// it, without touching any connection or making the refusal permanent
    /// the way [`Pool::close`] does.
    ///
    /// `docs/audits/fable-lifecycle.md`, finding 2: `MountRegistry::stop`
    /// calls this, then [`Pool::shutdown_open`], then joins the bridge's
    /// threads, then calls [`Pool::unpause`]. Pausing first is what stops a
    /// waiter `shutdown_open` wakes by shutting down the connection someone
    /// else was holding from dialing straight back into the peer that call
    /// is trying to stop talking to.
    pub(crate) fn pause(&self) {
        lock_mutex(&self.inner).paused = true;
        self.slot_freed.notify_all();
    }

    /// Lifts [`Pool::pause`]. Safe to call on a pool that was never paused.
    pub(crate) fn unpause(&self) {
        lock_mutex(&self.inner).paused = false;
    }

    /// Shuts down every socket this pool currently holds open, idle and
    /// borrowed, without closing or pausing the pool itself: a caller must
    /// call [`Pool::pause`] first if a waiter must not dial straight back
    /// in. A later `take` or `take_dialing`, once unpaused, still works,
    /// dialing fresh as needed.
    ///
    /// `docs/audits/fable-lifecycle.md`, finding 2: `MountRegistry::stop`
    /// used to join the bridge's prefetch thread with nothing to end its
    /// blocked read of a peer that stopped answering, or its own wait
    /// inside `take` for a free slot, so either could hold the join for as
    /// long as the wire's own idle timeout. This reuses the same shutdown
    /// [`Pool::close`] already does for its idle connections, extended to
    /// a connection borrowed out right now: [`Pooled`] registers its raw
    /// socket the same way every other transport in this engine does
    /// (`docs/engine-contract.md` item 16c), so the id recorded in
    /// `outstanding_ids` at borrow time is enough to find and shut it down
    /// here, the same call `Engine::stop` makes on its own registered
    /// sockets.
    ///
    /// A no-op if this pool has never dialed: there is nothing open yet,
    /// and so nothing yet to have learned `Shared` from.
    pub(crate) fn shutdown_open(&self) {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(shared) = inner.shared_for_shutdown.clone() else {
            return;
        };
        let mut ids: Vec<u64> = inner.idle.iter().map(|pooled| pooled.id).collect();
        ids.extend(inner.outstanding_ids.iter().copied());
        drop(inner);
        let sockets = lock_mutex(&shared.sockets);
        for id in ids {
            if let Some(socket) = sockets.get(&id) {
                drop(socket.shutdown(Shutdown::Both));
            }
        }
        drop(sockets);
        self.slot_freed.notify_all();
    }
}

/// One borrowed connection. Call [`Borrowed::mark_unhealthy`] before an RPC
/// error is allowed to propagate, so a dead connection is dropped instead
/// of handed to the next request.
pub(crate) struct Borrowed<'a> {
    pool: &'a Pool,
    client: Option<Pooled>,
    healthy: bool,
}

impl<'a> Borrowed<'a> {
    fn new(pool: &'a Pool, client: Pooled) -> Self {
        Self {
            pool,
            client: Some(client),
            healthy: true,
        }
    }

    pub(crate) fn client(&mut self) -> &mut PeerClient {
        &mut self
            .client
            .as_mut()
            .expect("taken exactly once, given back on drop")
            .client
    }

    /// Marks this connection as broken, so it is not returned to the pool.
    pub(crate) fn mark_unhealthy(&mut self) {
        self.healthy = false;
    }

    /// Runs one call on this connection and maps a failure to a
    /// [`FerryError`], marking the connection unhealthy when the failure
    /// was the connection rather than the peer.
    ///
    /// A [`RpcError::Remote`] is the peer's own answer, such as
    /// `OpError::NotFound`, so the connection is still good and goes back
    /// to the pool. Anything else means the stream broke, the peer stopped,
    /// or the cable went, so the connection is dropped instead of handed to
    /// the next caller. That is the same split
    /// `dav::server::map_write_error` makes for the bridge.
    ///
    /// # Errors
    ///
    /// Whatever [`crate::errors::from_rpc`] makes of `rpc`'s own error.
    pub(crate) fn call<T>(
        &mut self,
        rpc: impl FnOnce(&mut PeerClient) -> Result<T, RpcError>,
    ) -> Result<T, FerryError> {
        match rpc(self.client()) {
            Ok(value) => Ok(value),
            Err(error) => {
                if !matches!(error, RpcError::Remote(_)) {
                    self.mark_unhealthy();
                }
                Err(from_rpc(&error))
            }
        }
    }
}

impl Drop for Borrowed<'_> {
    fn drop(&mut self) {
        self.pool.give_back(self.client.take(), self.healthy);
    }
}
