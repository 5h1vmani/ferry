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
//! - `pool.rs`: a small pool of the engine's own [`ferry_core::rpc::Client`]
//!   connections to the peer, four at most.
//! - `put.rs`: item I2's landing rule for `PUT` of a real file and for
//!   `COPY`, reusing item 5's push rule.
//! - `delete.rs`: item I2's recursive `DELETE` plan, over `folder.rs`'s
//!   bounds.
//! - `server.rs`: the accept loop, the connection loop, and the verbs.

mod cache;
mod delete;
mod http;
mod lock;
mod pool;
mod probes;
mod put;
mod server;
mod xml;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::errors::failed_with;
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

/// One running bridge: the endpoint it answers as, and the handle to stop
/// it.
struct Mount {
    endpoint: MountEndpoint,
    port: u16,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Every device's `WebDAV` bridge, by device key hex.
///
/// One instance lives in [`crate::engine::Shared`] for the life of the
/// engine. Starting a bridge that is already running returns its existing
/// endpoint rather than opening a second port, so `Engine::mount_start` is
/// idempotent.
pub(crate) struct MountRegistry {
    mounts: Mutex<BTreeMap<String, Mount>>,
}

impl MountRegistry {
    pub(crate) fn new() -> Self {
        Self {
            mounts: Mutex::new(BTreeMap::new()),
        }
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
    /// bound or the password cannot be generated.
    pub(crate) fn start(
        &self,
        shared: &Arc<crate::engine::Shared>,
        device_key_hex: &str,
        device_name: &str,
    ) -> Result<MountEndpoint, FerryError> {
        let mut mounts = lock_mutex(&self.mounts);
        if let Some(mount) = mounts.get(device_key_hex) {
            return Ok(mount.endpoint.clone());
        }

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

        let running = Arc::new(AtomicBool::new(true));
        let bridge = Arc::new(server::Bridge::new(
            device_key_hex.to_owned(),
            device_name.to_owned(),
            user,
            password,
            port,
            shared.data_dir.join("dav_sidecars").join(device_key_hex),
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

        mounts.insert(
            device_key_hex.to_owned(),
            Mount {
                endpoint: endpoint.clone(),
                port,
                running,
                handle: Some(handle),
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
        if let Some(handle) = mount.handle.take() {
            drop(handle.join());
        }
    }

    /// Stop every bridge. Called by `Engine::stop`.
    pub(crate) fn stop_all(&self) {
        let keys: Vec<String> = lock_mutex(&self.mounts).keys().cloned().collect();
        for key in keys {
            self.stop(&key);
        }
    }
}
