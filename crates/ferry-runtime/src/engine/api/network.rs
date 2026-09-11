//! The Wi-Fi network the app reports it is on, and the trusted list its
//! presence is checked against.

use crate::FerryError;
use crate::engine::{Engine, notify, save_networks};
use crate::networks::apply_presence;
use crate::notify::Change;
use crate::state::lock;

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
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
}
