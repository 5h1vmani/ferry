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
        loop {
            if let Some(client) = inner.idle.pop() {
                inner.outstanding += 1;
                return Ok(Borrowed::new(self, client));
            }
            if inner.idle.len() + inner.outstanding < MAX_CONNECTIONS {
                inner.outstanding += 1;
                drop(inner);
                return match self.dial(shared, require_reachable) {
                    Ok(client) => Ok(Borrowed::new(self, client)),
                    Err(error) => {
                        self.give_back(None);
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

    /// Takes back a borrowed connection. `None` means it was found broken
    /// and is dropped rather than reused.
    fn give_back(&self, client: Option<Pooled>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.outstanding = inner.outstanding.saturating_sub(1);
        if let Some(client) = client {
            inner.idle.push(client);
        }
        drop(inner);
        self.slot_freed.notify_one();
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
        let client = if self.healthy {
            self.client.take()
        } else {
            None
        };
        self.pool.give_back(client);
    }
}
