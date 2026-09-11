//! The trusted Wi-Fi networks, and the two rules that decide what this
//! device does over Wi-Fi.
//!
//! `docs/engine-contract.md`, item 18. A device that paired at home stays
//! silent in a café. The app reads the Wi-Fi network name, because only the
//! app can. This module decides what to do with it, and nothing else
//! decides.
//!
//! This module owns two things and nothing more. It owns the file
//! `data_dir/networks`, which holds the trusted names, one name per line in
//! UTF-8. It owns [`browse_allowed`], which does not need `reachable`, and
//! [`wifi_presence`], which does.
//!
//! The file is written the way `record.rs` and `auto_copy.rs` write theirs:
//! the bytes go to a temporary name, are flushed to the disk, and only then
//! renamed over the real name. A rename is one step, so the real name always
//! holds either the old list or the new one, never half of either. The file
//! is created in mode `0o600` on Unix, as `auto_copy.rs` creates its own: a
//! list of the networks a person is on says where they have been.
//!
//! A missing file is an empty list. A file this build cannot read is an
//! empty list too. An empty list trusts every network, which is what a
//! person who never granted the location permission already has, so a lost
//! file costs nothing but the names.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ferry_core::session::SessionId;

use crate::FerryError;
use crate::errors::failed;
use crate::state::State;

/// The trusted list holds at most this many names, and each name holds at
/// most this many bytes.
///
/// `docs/engine-contract.md`, item 18. One number, because the contract
/// names one: a person hand-trusting a 33rd network, or a name longer than
/// this, is refused with `Runtime::NetworkName`.
pub(crate) const NETWORK_LIMIT: usize = 32;

/// The name of the file under the data directory.
const FILE_NAME: &str = "networks";

/// True when this device should browse for other devices over Wi-Fi.
///
/// The rule, with every input as an argument, so a test can run one case per
/// branch without building an engine. [`browse_allowed`] is the only caller
/// that matters; it reads the inputs off the engine's state.
///
/// Browsing is allowed when one of three things holds: the trusted list is
/// empty, the current network is in the list, or pairing is in progress. An
/// unknown network with a non-empty list is not. This does not read
/// `reachable`: a browse query is quiet enough to run on any network, and a
/// Mac with its presence switch off must still find and mount a phone, as it
/// did before this item.
#[doc(hidden)]
#[must_use]
pub fn browse_allowed_rule(
    trusted: &[String],
    network: Option<&str>,
    pairing_in_progress: bool,
) -> bool {
    trusted.is_empty()
        || network.is_some_and(|name| trusted.iter().any(|known| known == name))
        || pairing_in_progress
}

/// True when this device should advertise and accept over Wi-Fi.
///
/// The rule, with every input as an argument, so a test can run one case per
/// branch without building an engine. [`wifi_presence`] is the only caller
/// that matters; it reads the inputs off the engine's state.
///
/// Presence is on when `reachable` is on and [`browse_allowed_rule`] is
/// true. So a person who never granted the location permission sees no
/// change from before this item, and a person who granted it once is quiet
/// on every network they did not pair on or trust by hand.
#[doc(hidden)]
#[must_use]
pub fn wifi_presence_rule(
    reachable: bool,
    trusted: &[String],
    network: Option<&str>,
    pairing_in_progress: bool,
) -> bool {
    reachable && browse_allowed_rule(trusted, network, pairing_in_progress)
}

/// True when this device should browse for other devices over Wi-Fi.
///
/// The one place [`browse_allowed_rule`] is read off the engine's state.
/// `engine::apply_presence` is what turns the answer into a browse loop that
/// holds a `Browser`. Unlike [`wifi_presence`], this does not gate on
/// `reachable`, so the person's own switch never silences browsing.
pub(crate) fn browse_allowed(state: &State) -> bool {
    browse_allowed_rule(
        state.trusted.names(),
        state.network.as_deref(),
        state.pairing.is_running(),
    )
}

/// True when this device should advertise and accept over Wi-Fi.
///
/// The one place the rule is read off the engine's state. Everything that
/// acts on presence calls this, and `engine::apply_presence` is what turns
/// the answer into a running advertiser.
///
/// "Pairing is in progress" is `Pairing::is_running`, which is every pairing
/// state but `Idle`, `Confirmed`, and `Failed`.
pub(crate) fn wifi_presence(state: &State) -> bool {
    state.reachable && browse_allowed(state)
}

/// The trusted Wi-Fi network names, and the file they live in.
#[derive(Debug, Clone)]
pub(crate) struct TrustedNetworks {
    /// `data_dir/networks`.
    path: PathBuf,
    /// The names, in the order they were trusted, oldest first.
    names: Vec<String>,
}

impl TrustedNetworks {
    /// Read the list under `data_dir`.
    ///
    /// Never fails. A missing file, an unreadable file, or a file that is
    /// not UTF-8 all give an empty list.
    pub(crate) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE_NAME);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let mut names: Vec<String> = Vec::new();
        for line in text.lines() {
            // A line this build would refuse to add is dropped rather than
            // kept, so what is loaded is always a list this build could have
            // written itself.
            if line.is_empty() || line.len() > NETWORK_LIMIT || names.len() >= NETWORK_LIMIT {
                continue;
            }
            if !names.iter().any(|known| known == line) {
                names.push(line.to_owned());
            }
        }
        Self { path, names }
    }

    /// The names, oldest first.
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    /// Add `name` and write the file. Returns true when the list changed.
    ///
    /// A name already in the list is not an error and is not written again,
    /// so trusting the same network twice costs no disk write and fires no
    /// callback.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NetworkName` for an empty name, a name over
    /// [`NETWORK_LIMIT`] bytes, or a name that would be the
    /// [`NETWORK_LIMIT`] plus first. Returns `TransferError::Local` when
    /// local storage refuses the write.
    pub(crate) fn add(&mut self, name: &str) -> Result<bool, FerryError> {
        if name.is_empty() || name.len() > NETWORK_LIMIT {
            return Err(failed("Runtime::NetworkName"));
        }
        if self.names.iter().any(|known| known == name) {
            return Ok(false);
        }
        if self.names.len() >= NETWORK_LIMIT {
            return Err(failed("Runtime::NetworkName"));
        }
        self.names.push(name.to_owned());
        if let Err(error) = self.save() {
            // The file is the truth this device starts from, so a list that
            // was not written is not a list this device holds.
            self.names.pop();
            return Err(error);
        }
        Ok(true)
    }

    /// Remove `name` and write the file. Returns true when the list changed.
    ///
    /// A name that is not in the list is not an error: forgetting a network
    /// twice leaves the same list both times.
    ///
    /// # Errors
    ///
    /// Returns `TransferError::Local` when local storage refuses the write.
    pub(crate) fn remove(&mut self, name: &str) -> Result<bool, FerryError> {
        let Some(at) = self.names.iter().position(|known| known == name) else {
            return Ok(false);
        };
        let removed = self.names.remove(at);
        if let Err(error) = self.save() {
            self.names.insert(at, removed);
            return Err(error);
        }
        Ok(true)
    }

    /// Write every name to the file, one per line.
    fn save(&self) -> Result<(), FerryError> {
        let mut text = String::new();
        for name in &self.names {
            text.push_str(name);
            text.push('\n');
        }
        write_private_file(&self.path, text.as_bytes())
    }
}

/// Write `bytes` at `path` through a temporary name and a rename.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), FerryError> {
    let temporary = temporary_name(path)?;
    let written = write_and_sync(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        // A temporary file that is left behind is never read again, but it
        // would sit in the app's own folder for ever.
        drop(fs::remove_file(&temporary));
        return Err(failed("TransferError::Local"));
    }
    Ok(())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = open_new_private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Create `path` in mode `0o600` on Unix, the same private mode
/// `auto_copy.rs` gives its own file, set as part of the same syscall that
/// creates it so there is no moment where the file exists with a wider mode.
/// `create_new` refuses to write through anything already at the name,
/// including a symbolic link someone planted there.
#[cfg(unix)]
fn open_new_private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_new_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// A name next to `path` that nothing can be waiting at.
fn temporary_name(path: &Path) -> Result<PathBuf, FerryError> {
    let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{session}.tmp"));
    Ok(PathBuf::from(name))
}
