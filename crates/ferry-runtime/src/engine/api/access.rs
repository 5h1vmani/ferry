//! Reading back the access log the apps display.

use crate::AccessEntry;
use crate::engine::{Engine, access_entry_from_core};
use crate::state::lock;

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
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

    /// How many days an access log entry is kept before it is pruned.
    ///
    /// `docs/engine-contract.md`, batch E, item 13. The app formats "Kept
    /// for {days} days" from this number, rather than typing 30 itself.
    #[must_use]
    pub fn access_log_retention_days(&self) -> u32 {
        u32::try_from(crate::access::RETENTION_DAYS).unwrap_or(u32::MAX)
    }
}
