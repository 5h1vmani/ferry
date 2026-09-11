//! Job 7's automatic copy switch for one device: reading it back and
//! turning it on or off.

use crate::engine::Engine;
use crate::{AutoCopy, FerryError};

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
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
}
