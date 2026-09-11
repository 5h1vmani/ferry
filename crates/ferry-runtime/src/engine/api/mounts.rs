//! Starting and stopping a device's `WebDAV` bridge, and recording
//! where the app mounted it.

use crate::engine::{Engine, notify};
use crate::errors::failed;
use crate::notify::Change;
use crate::state::{key_from_hex, lock};
use crate::{FerryError, MountEndpoint};

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
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
}
