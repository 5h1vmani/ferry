//! The devices this one has paired with, and where its own key lives.
//!
//! This module is implemented. The contract below is the specification.
//!
//! # Contract
//!
//! Two things live here because they are always used together.
//!
//! `PeerStore` holds the paired devices. It persists through the wire codec
//! to one file. Removing a peer is how a device is forgotten. The caller is
//! responsible for deleting that peer's session manifests afterwards, because
//! sessions are another module's job.
//!
//! `SecretStore` is a trait for where this device's own static key lives. The
//! real implementations are the macOS Keychain and Android app-private storage,
//! and they live in the apps. `FileSecretStore` is the development
//! implementation. It writes the key to a plain file and its documentation
//! says, in the first sentence, that it is not for real use.
//!
//! Public shape:
//!
//! ```text
//! pub struct Peer { pub key: PublicKey, pub name: String, pub paired_unix_secs: i64 }
//!
//! pub struct PeerStore { .. }
//! impl PeerStore {
//!     pub fn load(path: &Path) -> Result<Self, PeerError>;   // missing file is an empty store
//!     pub fn save(&self) -> Result<(), PeerError>;            // writes to a temp name, then renames
//!     pub fn add(&mut self, peer: Peer);                       // replaces an existing entry for the same key
//!     pub fn remove(&mut self, key: &PublicKey) -> Option<Peer>;
//!     pub fn get(&self, key: &PublicKey) -> Option<&Peer>;
//!     pub fn all(&self) -> &[Peer];
//! }
//!
//! pub trait SecretStore {
//!     fn load(&self) -> Result<Option<StaticKey>, PeerError>;
//!     fn save(&self, key: &StaticKey) -> Result<(), PeerError>;
//! }
//!
//! pub struct FileSecretStore { .. }
//! impl FileSecretStore { pub fn new(path: PathBuf) -> Self; }
//! impl SecretStore for FileSecretStore { .. }
//! ```
//!
//! The peer file has a version byte first, so a later format can be told
//! apart. A name is at most 256 bytes. The store holds at most 64 peers.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use zeroize::Zeroize;

use crate::noise::{NoiseError, PublicKey, StaticKey};
use crate::wire::{Decoder, Encoder, WireError};

/// The only format version this build writes or reads.
const FORMAT_VERSION: u8 = 1;

/// The most peers a store may hold. See the module documentation.
const MAX_PEERS: usize = 64;

/// The most bytes a peer name may take. See the module documentation.
const MAX_NAME_LEN: usize = 256;

/// The reason a peer store or secret store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// The filesystem refused something.
    #[error("storage failed: {0}")]
    Io(#[from] io::Error),
    /// The stored bytes did not decode.
    #[error("the stored data did not decode: {0}")]
    Wire(#[from] WireError),
    /// A stored key was not one `StaticKey::from_stored` will accept.
    #[error("the stored key is unusable: {0}")]
    BadKey(#[from] NoiseError),
    /// The format version byte named a format this build does not know.
    #[error("format version {0} is not one this build knows")]
    UnknownFormat(u8),
    /// The store would hold more than the 64 peer limit.
    #[error("the store cannot hold more than 64 peers")]
    TooMany,
    /// A peer name is over the 256 byte limit.
    #[error("a peer name is over the 256 byte limit")]
    NameTooLong,
}

/// One device this device has paired with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// The device's static public key.
    pub key: PublicKey,
    /// The name shown to the user for this device.
    pub name: String,
    /// When the two devices paired, in Unix seconds.
    pub paired_unix_secs: i64,
}

/// The paired devices, persisted to one file.
///
/// Peers are kept in a `BTreeMap` keyed by public key rather than a `Vec`.
/// The map keeps its entries in key order by itself, so `all` needs no sort
/// step and stays deterministic. `add`, `get`, and `remove` are then direct
/// map lookups, instead of a hand written binary search over a `Vec` that
/// the caller would have to keep sorted.
#[derive(Debug, Clone)]
pub struct PeerStore {
    path: PathBuf,
    peers: BTreeMap<PublicKey, Peer>,
}

impl PeerStore {
    /// Load the store from `path`.
    ///
    /// A missing file is not an error. It is treated as an empty store,
    /// bound to `path` so a later `save` creates the file.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::Io`] for a filesystem error other than the file
    /// being missing, [`PeerError::UnknownFormat`] when the version byte
    /// names a format this build does not know, [`PeerError::TooMany`] when
    /// the stored count is over the limit, and [`PeerError::Wire`] when the
    /// bytes are otherwise malformed.
    pub fn load(path: &Path) -> Result<Self, PeerError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: path.to_path_buf(),
                    peers: BTreeMap::new(),
                });
            }
            Err(err) => return Err(PeerError::Io(err)),
        };
        let peers = decode_peers(&bytes)?;
        Ok(Self {
            path: path.to_path_buf(),
            peers,
        })
    }

    /// Write the store out, replacing whatever was there before.
    ///
    /// The bytes go to a temporary name next to `path` first, then that file
    /// is renamed over `path`. A crash mid write leaves the temporary file
    /// behind and never a half written store.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::NameTooLong`] when a stored name is over the
    /// limit, [`PeerError::TooMany`] when the store holds more peers than
    /// the format allows, and [`PeerError::Io`] when the filesystem refuses
    /// the write.
    pub fn save(&self) -> Result<(), PeerError> {
        if self.peers.len() > MAX_PEERS {
            return Err(PeerError::TooMany);
        }
        for peer in self.peers.values() {
            if peer.name.len() > MAX_NAME_LEN {
                return Err(PeerError::NameTooLong);
            }
        }
        let bytes = encode_peers(&self.peers);
        write_private_file(&self.path, &bytes)
    }

    /// Add a peer, replacing any existing entry for the same key.
    pub fn add(&mut self, peer: Peer) {
        self.peers.insert(peer.key, peer);
    }

    /// Remove a peer, returning it if it was present.
    pub fn remove(&mut self, key: &PublicKey) -> Option<Peer> {
        self.peers.remove(key)
    }

    /// Look up a peer by key.
    #[must_use]
    pub fn get(&self, key: &PublicKey) -> Option<&Peer> {
        self.peers.get(key)
    }

    /// Every stored peer, in key order.
    ///
    /// This returns an owned `Vec` rather than a slice, because the peers
    /// live in a `BTreeMap` and not in a `Vec` of their own. See the
    /// `PeerStore` documentation for why.
    #[must_use]
    pub fn all(&self) -> Vec<Peer> {
        self.peers.values().cloned().collect()
    }
}

// Read the stored peer list. A wrong version byte or an over-large count is
// caught before anything else is read, so a corrupted or hostile file cannot
// make this allocate more than the limit allows.
fn decode_peers(bytes: &[u8]) -> Result<BTreeMap<PublicKey, Peer>, PeerError> {
    let mut d = Decoder::new(bytes);
    let version = d.u8()?;
    if version != FORMAT_VERSION {
        return Err(PeerError::UnknownFormat(version));
    }
    let count = d.u32()?;
    if count as usize > MAX_PEERS {
        return Err(PeerError::TooMany);
    }
    let mut peers = BTreeMap::new();
    for _ in 0..count {
        let key = PublicKey(d.fixed::<32>()?);
        let name = d.text(MAX_NAME_LEN)?.to_string();
        let paired_unix_secs = decode_i64(d.u64()?);
        peers.insert(
            key,
            Peer {
                key,
                name,
                paired_unix_secs,
            },
        );
    }
    d.finish()?;
    Ok(peers)
}

// Write the peer list out in file order. The caller checks the name and
// count limits first, so this never has to reject anything.
fn encode_peers(peers: &BTreeMap<PublicKey, Peer>) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(FORMAT_VERSION);
    let count = u32::try_from(peers.len()).unwrap_or(u32::MAX);
    e.u32(count);
    for peer in peers.values() {
        e.fixed(peer.key.as_bytes());
        e.text(&peer.name);
        e.u64(encode_i64(peer.paired_unix_secs));
    }
    e.finish()
}

// `Encoder` and `Decoder` have no signed integer methods, so a Unix second
// count is carried as its bit pattern instead. An `as` cast between `i64`
// and `u64` would be a truncating cast in clippy's eyes even though no bits
// are lost, so the bits are reinterpreted explicitly. `ops.rs` does the same.
fn encode_i64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

fn decode_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

/// Where this device's own static key lives.
///
/// The real implementations are the macOS Keychain and Android's
/// app-private storage. Those live in the apps, not in this crate, since
/// this crate does not depend on either platform. [`FileSecretStore`] is the
/// development stand-in used until then.
pub trait SecretStore {
    /// Read the stored key, if one exists.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError`] when a stored key exists but cannot be read, or
    /// does not decode into a usable key.
    fn load(&self) -> Result<Option<StaticKey>, PeerError>;

    /// Store the key, replacing whatever was stored before.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError`] when the key cannot be written.
    fn save(&self, key: &StaticKey) -> Result<(), PeerError>;
}

/// Stores this device's own static key in a plain file, in the clear. This
/// is a development convenience, not something a real device should use.
///
/// A real build must use the macOS Keychain or Android's app-private
/// storage instead, through another implementation of [`SecretStore`]. Those
/// live in the apps because this crate cannot depend on either platform.
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    /// Point at where the key should live.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl SecretStore for FileSecretStore {
    fn load(&self) -> Result<Option<StaticKey>, PeerError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(PeerError::Io(err)),
        };
        let mut d = Decoder::new(&bytes);
        let mut private = d.fixed::<32>()?;
        let public = d.fixed::<32>()?;
        d.finish()?;
        // The key is built before the buffer is wiped, since `StaticKey`
        // keeps its own copy and zeroizes that copy on drop.
        let key = StaticKey::from_stored(&private, &public);
        private.zeroize();
        Ok(Some(key?))
    }

    fn save(&self, key: &StaticKey) -> Result<(), PeerError> {
        let mut private = *key.private_bytes();
        let mut e = Encoder::new();
        e.fixed(&private);
        e.fixed(key.public().as_bytes());
        let bytes = e.finish();
        let result = write_private_file(&self.path, &bytes);
        private.zeroize();
        result
    }
}

// Write `bytes` to `path` so a crash mid write can never leave a half file
// at `path`. The bytes go to a temporary name next to `path`, and that file
// is renamed over `path` only once the write has finished. Both callers
// store secrets, so the temporary file is restricted to its owner before any
// bytes reach it.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), PeerError> {
    let tmp = tmp_path(path);
    fs::File::create(&tmp)?;
    set_owner_only(&tmp)?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// The temporary name `write_private_file` writes to before renaming.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

// Restrict a file to its own owner. A no-op on platforms with no Unix
// permission bits.
#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), PeerError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<(), PeerError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FileSecretStore, Peer, PeerError, PeerStore, SecretStore};
    use crate::noise::{PublicKey, StaticKey};
    use crate::wire::WireError;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Every test gets its own directory under the system temp directory, so
    // tests running in parallel in the same process never share a path.
    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(label: &str) -> PathBuf {
        let n = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("ferry-peers-test-{pid}-{label}-{n}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    fn sample_peer(byte: u8, name: &str, paired_unix_secs: i64) -> Peer {
        Peer {
            key: key(byte),
            name: name.to_string(),
            paired_unix_secs,
        }
    }

    #[test]
    fn an_empty_store_loads_from_a_missing_file_and_saves_to_create_it() {
        let dir = unique_temp_dir("empty_store");
        let path = dir.join("peers.bin");

        let store = PeerStore::load(&path).unwrap();
        assert!(store.all().is_empty());
        assert!(!path.exists());

        store.save().unwrap();
        assert!(path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_then_save_then_load_round_trips() {
        let dir = unique_temp_dir("round_trip");
        let path = dir.join("peers.bin");

        let mut store = PeerStore::load(&path).unwrap();
        store.add(sample_peer(1, "Shiva's MacBook", 1_700_000_000));
        store.add(sample_peer(2, "Shiva's Pixel", 1_700_000_500));
        store.save().unwrap();

        let loaded = PeerStore::load(&path).unwrap();
        assert_eq!(loaded.all(), store.all());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_with_an_existing_key_replaces_the_entry() {
        let dir = unique_temp_dir("replace");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path).unwrap();

        store.add(sample_peer(1, "old-name", 100));
        store.add(sample_peer(1, "new-name", 200));

        assert_eq!(store.all().len(), 1);
        assert_eq!(store.get(&key(1)).unwrap().name, "new-name");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_returns_the_peer_and_a_second_remove_returns_none() {
        let dir = unique_temp_dir("remove");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path).unwrap();
        store.add(sample_peer(1, "device", 100));

        let removed = store.remove(&key(1));
        assert_eq!(removed, Some(sample_peer(1, "device", 100)));
        assert_eq!(store.remove(&key(1)), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_over_256_bytes_is_refused() {
        let dir = unique_temp_dir("long_name");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path).unwrap();
        let long_name = "a".repeat(257);
        store.add(sample_peer(1, &long_name, 100));

        assert!(matches!(store.save(), Err(PeerError::NameTooLong)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tampered_format_version_byte_is_refused() {
        let dir = unique_temp_dir("bad_version");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path).unwrap();
        store.add(sample_peer(1, "device", 100));
        store.save().unwrap();

        let mut bytes = fs::read(&path).unwrap();
        bytes[0] = 99;
        fs::write(&path, &bytes).unwrap();

        match PeerStore::load(&path) {
            Err(PeerError::UnknownFormat(99)) => {}
            other => panic!("expected UnknownFormat(99), got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let dir = unique_temp_dir("trailing");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path).unwrap();
        store.add(sample_peer(1, "device", 100));
        store.save().unwrap();

        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            PeerStore::load(&path),
            Err(PeerError::Wire(WireError::TrailingBytes))
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_secret_store_round_trips_a_generated_key() {
        let dir = unique_temp_dir("secret_round_trip");
        let path = dir.join("key.bin");
        let store = FileSecretStore::new(path);

        let generated = StaticKey::generate().unwrap();
        store.save(&generated).unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.public(), generated.public());
        assert_eq!(loaded.private_bytes(), generated.private_bytes());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_secret_store_load_on_a_missing_file_returns_ok_none() {
        let dir = unique_temp_dir("secret_missing");
        let path = dir.join("key.bin");
        let store = FileSecretStore::new(path);

        assert!(store.load().unwrap().is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn on_unix_both_files_are_created_with_mode_0o600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("permissions");
        let peers_path = dir.join("peers.bin");
        let secret_path = dir.join("key.bin");

        let mut store = PeerStore::load(&peers_path).unwrap();
        store.add(sample_peer(1, "device", 100));
        store.save().unwrap();

        let secret_store = FileSecretStore::new(secret_path.clone());
        let generated = StaticKey::generate().unwrap();
        secret_store.save(&generated).unwrap();

        let peers_mode = fs::metadata(&peers_path).unwrap().permissions().mode() & 0o777;
        let secret_mode = fs::metadata(&secret_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(peers_mode, 0o600);
        assert_eq!(secret_mode, 0o600);

        let _ = fs::remove_dir_all(&dir);
    }
}
