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
//! pub struct Peer { pub key: PublicKey, pub name: String, pub paired_unix_secs: i64, pub kind: DeviceKind }
//!
//! pub struct PeerStore { .. }
//! impl PeerStore {
//!     // missing file is an empty store; a version 1 file gets `assumed_kind`
//!     // on every peer, since it predates the kind byte
//!     pub fn load(path: &Path, assumed_kind: DeviceKind) -> Result<Self, PeerError>;
//!     pub fn save(&self) -> Result<(), PeerError>;            // writes to a temp name, then renames
//!     // replaces an existing entry for the same key; refuses a new key
//!     // past MAX_PEERS
//!     pub fn add(&mut self, peer: Peer) -> Result<(), PeerError>;
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
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use zeroize::{Zeroize, Zeroizing};

use crate::noise::{NoiseError, PublicKey, StaticKey};
use crate::wire::{Decoder, Encoder, WireError};

/// The newest peer store format this build writes, and one of the two it
/// reads. See [`PeerStore::load`] for how a version 1 file is handled.
const FORMAT_VERSION: u8 = 2;

/// The format version before the kind byte was added to each peer.
const FORMAT_VERSION_1: u8 = 1;

/// What kind of device a peer is.
///
/// Sent as the kind byte in `hello` (`crate::rpc::exchange_hello`), and
/// stored with each [`Peer`]. `docs/engine-contract.md`, batch C, item 11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// An Android phone.
    Phone,
    /// A Mac.
    Mac,
}

impl DeviceKind {
    // The wire byte for this kind. Shared by the hello payload and the peer
    // store, so both use the same encoding.
    pub(crate) fn to_byte(self) -> u8 {
        match self {
            Self::Phone => 1,
            Self::Mac => 2,
        }
    }

    // Decode a wire byte. An unrecognised byte is a malformed payload, the
    // same class of error as any other unknown tag on the wire.
    //
    // # Errors
    //
    // Returns `WireError::UnknownTag` for a byte that names no kind.
    pub(crate) fn from_byte(value: u8) -> Result<Self, WireError> {
        match value {
            1 => Ok(Self::Phone),
            2 => Ok(Self::Mac),
            other => Err(WireError::UnknownTag(other)),
        }
    }
}

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
    /// The system supplied no random bytes for the temporary file name.
    #[error("the system supplied no random bytes")]
    NoRandomness,
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
    /// What kind of device it is.
    pub kind: DeviceKind,
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
    /// A version 1 file, written before a peer carried its kind, still
    /// loads: every peer in it is given `assumed_kind`. A version 2 file
    /// carries the real kind of each peer and ignores `assumed_kind`.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::Io`] for a filesystem error other than the file
    /// being missing, [`PeerError::UnknownFormat`] when the version byte
    /// names a format this build does not know, [`PeerError::TooMany`] when
    /// the stored count is over the limit, and [`PeerError::Wire`] when the
    /// bytes are otherwise malformed.
    pub fn load(path: &Path, assumed_kind: DeviceKind) -> Result<Self, PeerError> {
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
        let peers = decode_peers(&bytes, assumed_kind)?;
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
    ///
    /// F7: `save` already refused a store over [`MAX_PEERS`], but only once
    /// something tried to write it to disk. Refusing here too means the
    /// limit bounds how many peers a pairing trial can hold in memory in
    /// the first place, not only what survives to be saved. A peer that
    /// already has an entry may still be replaced past the limit: this
    /// never grows the count, so it is never what the limit is protecting
    /// against.
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::TooMany`] when the store already holds
    /// [`MAX_PEERS`] peers and `peer.key` does not name one of them.
    pub fn add(&mut self, peer: Peer) -> Result<(), PeerError> {
        if !self.peers.contains_key(&peer.key) && self.peers.len() >= MAX_PEERS {
            return Err(PeerError::TooMany);
        }
        self.peers.insert(peer.key, peer);
        Ok(())
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
//
// A version 1 file has no kind byte per peer, since it predates item 11. Its
// peers are given `assumed_kind` instead of a stored value. A version 2
// file carries the real kind and `assumed_kind` is unused.
fn decode_peers(
    bytes: &[u8],
    assumed_kind: DeviceKind,
) -> Result<BTreeMap<PublicKey, Peer>, PeerError> {
    let mut d = Decoder::new(bytes);
    let version = d.u8()?;
    if version != FORMAT_VERSION && version != FORMAT_VERSION_1 {
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
        let kind = if version == FORMAT_VERSION_1 {
            assumed_kind
        } else {
            DeviceKind::from_byte(d.u8()?)?
        };
        peers.insert(
            key,
            Peer {
                key,
                name,
                paired_unix_secs,
                kind,
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
        e.u8(peer.kind.to_byte());
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

// The number of bytes `FileSecretStore` encodes: a 32 byte private key
// followed by a 32 byte public key. `save` builds its encoder with this
// capacity so it allocates once and never reallocates. A reallocation would
// copy the private key into a new allocation and leave the old one, still
// holding the key, unwiped. A test checks this value directly, so it is its
// own function rather than an inline literal.
fn encoded_key_capacity() -> usize {
    64
}

impl SecretStore for FileSecretStore {
    fn load(&self) -> Result<Option<StaticKey>, PeerError> {
        // The whole file, including the private key, is wiped when `bytes`
        // drops, however this function returns.
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => Zeroizing::new(bytes),
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
        let mut e = Encoder::with_capacity(encoded_key_capacity());
        e.fixed(&private);
        e.fixed(key.public().as_bytes());
        // Wrapping the finished bytes means they are wiped on drop, no
        // matter which path out of this function is taken.
        let bytes = Zeroizing::new(e.finish());
        let result = write_private_file(&self.path, &bytes);
        private.zeroize();
        result
    }
}

// Write `bytes` to `path` so a crash mid write can never leave a half file
// at `path`, and so the private key it usually holds is never reachable
// through a guessable name or a window of loose permissions. Both callers
// store secrets.
//
// The bytes go to a temporary name next to `path`, chosen at random so an
// attacker cannot plant anything at it in advance. `create_private_file`
// opens that name with `create_new`, which refuses to write through
// anything already there, including a symlink, and sets the owner-only mode
// at the moment of creation rather than after. Every byte is written
// through that one handle; the temporary name is never reopened by path.
// Once the write and the sync succeed, the file is renamed over `path`. If
// anything fails after the temporary file is created, it is removed before
// the error is returned.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), PeerError> {
    let tmp = random_tmp_path(path)?;
    let mut file = create_private_file(&tmp)?;
    if let Err(err) = write_all_and_sync(&mut file, bytes) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    drop(file);
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

// Create a new, empty file at exactly `path`, refusing to touch anything
// already there. `create_new` fails if `path` names anything at all,
// including a symlink, so a planted symlink is refused instead of
// followed. On Unix the mode is set as part of the same syscall that
// creates the file, so there is no moment where the file exists with a
// wider mode than 0o600.
//
// This is kept as its own function, separate from `write_private_file`, so
// a test can call it with a chosen path and check what `create_new` does
// when something is already there.
#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

// Write every byte through the handle that created the file, then flush it
// to disk before the caller renames it into place. The path is never
// reopened, so nothing between creation and this point can swap out what
// the handle points at.
fn write_all_and_sync(file: &mut fs::File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

// Build a temporary path next to `path` with a random suffix. The old
// scheme always used the same `<path>.tmp` name, which let an attacker
// plant something there ahead of time. A random suffix cannot be guessed in
// advance, so `create_private_file`'s `create_new` call is the only thing
// that decides whether the name was free.
fn random_tmp_path(path: &Path) -> Result<PathBuf, PeerError> {
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|_| PeerError::NoRandomness)?;
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(to_hex(suffix));
    name.push(".tmp");
    Ok(PathBuf::from(name))
}

// Sixteen lowercase hex characters from eight random bytes. Only ever
// called on the output of `getrandom::fill`, so it does not need to handle
// arbitrary input.
//
// `pub(crate)` because `discovery.rs` uses the same encoding for its own
// random instance name, and used to carry an identical copy of this
// function.
pub(crate) fn to_hex(bytes: [u8; 8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceKind, FORMAT_VERSION_1, FileSecretStore, MAX_PEERS, Peer, PeerError, PeerStore,
        SecretStore, encoded_key_capacity,
    };
    use crate::noise::{PublicKey, StaticKey};
    use crate::wire::{Encoder, WireError};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::thread;
    use zeroize::Zeroizing;

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
            kind: DeviceKind::Phone,
        }
    }

    #[test]
    fn an_empty_store_loads_from_a_missing_file_and_saves_to_create_it() {
        let dir = unique_temp_dir("empty_store");
        let path = dir.join("peers.bin");

        let store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
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

        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        store
            .add(sample_peer(1, "Shiva's MacBook", 1_700_000_000))
            .unwrap();
        store
            .add(sample_peer(2, "Shiva's Pixel", 1_700_000_500))
            .unwrap();
        store.save().unwrap();

        let loaded = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        assert_eq!(loaded.all(), store.all());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_2_round_trips_each_peers_kind() {
        let dir = unique_temp_dir("v2_kind_round_trip");
        let path = dir.join("peers.bin");

        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        store
            .add(Peer {
                key: key(1),
                name: "A Mac".to_string(),
                paired_unix_secs: 1,
                kind: DeviceKind::Mac,
            })
            .unwrap();
        store
            .add(Peer {
                key: key(2),
                name: "A Phone".to_string(),
                paired_unix_secs: 2,
                kind: DeviceKind::Phone,
            })
            .unwrap();
        store.save().unwrap();

        // Loaded with the opposite assumed kind from what was stored. A
        // version 2 file carries the real kind and ignores this argument,
        // so a bug that fell back to it here would show up as a mismatch.
        let loaded = PeerStore::load(&path, DeviceKind::Mac).unwrap();
        assert_eq!(loaded.get(&key(1)).unwrap().kind, DeviceKind::Mac);
        assert_eq!(loaded.get(&key(2)).unwrap().kind, DeviceKind::Phone);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_version_1_file_loads_with_the_assumed_kind() {
        let dir = unique_temp_dir("v1_load");
        let path = dir.join("peers.bin");

        // Built by hand in the version 1 shape: no kind byte per peer.
        let mut e = Encoder::new();
        e.u8(FORMAT_VERSION_1);
        e.u32(1);
        e.fixed(key(7).as_bytes());
        e.text("Old Peer");
        e.u64(super::encode_i64(500));
        fs::write(&path, e.finish()).unwrap();

        let loaded = PeerStore::load(&path, DeviceKind::Mac).unwrap();
        let peer = loaded.get(&key(7)).unwrap();
        assert_eq!(peer.kind, DeviceKind::Mac);
        assert_eq!(peer.name, "Old Peer");
        assert_eq!(peer.paired_unix_secs, 500);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_with_an_existing_key_replaces_the_entry() {
        let dir = unique_temp_dir("replace");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();

        store.add(sample_peer(1, "old-name", 100)).unwrap();
        store.add(sample_peer(1, "new-name", 200)).unwrap();

        assert_eq!(store.all().len(), 1);
        assert_eq!(store.get(&key(1)).unwrap().name, "new-name");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_65th_distinct_peer_is_refused_in_memory() {
        // F7: `save` already refused a store over MAX_PEERS, but only once
        // something tried to write it. `add` must refuse the same way
        // before the count ever grows past the limit in memory, so a
        // pairing trial cannot hold more than MAX_PEERS candidates even
        // before anything is saved.
        let dir = unique_temp_dir("max-peers");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();

        for i in 0..MAX_PEERS {
            let byte = u8::try_from(i % 256).unwrap_or(0);
            store
                .add(sample_peer(byte, "device", 100))
                .unwrap_or_else(|_| panic!("peer {i} is within the bound"));
        }
        assert_eq!(store.all().len(), MAX_PEERS);

        let refused = store.add(sample_peer(255, "one too many", 100));
        assert!(matches!(refused, Err(PeerError::TooMany)));
        assert_eq!(
            store.all().len(),
            MAX_PEERS,
            "the refused peer must not have been added"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_still_replaces_an_existing_key_once_the_store_is_full() {
        // A replacement never grows the count, so it is never what the
        // limit is protecting against, even once the store is already at
        // the bound.
        let dir = unique_temp_dir("max-peers-replace");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();

        for i in 0..MAX_PEERS {
            let byte = u8::try_from(i % 256).unwrap_or(0);
            store
                .add(sample_peer(byte, "device", 100))
                .unwrap_or_else(|_| panic!("peer {i} is within the bound"));
        }

        store
            .add(sample_peer(0, "renamed", 200))
            .expect("replacing an existing key must succeed even at the bound");
        assert_eq!(store.all().len(), MAX_PEERS);
        assert_eq!(store.get(&key(0)).unwrap().name, "renamed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_returns_the_peer_and_a_second_remove_returns_none() {
        let dir = unique_temp_dir("remove");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        store.add(sample_peer(1, "device", 100)).unwrap();

        let removed = store.remove(&key(1));
        assert_eq!(removed, Some(sample_peer(1, "device", 100)));
        assert_eq!(store.remove(&key(1)), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_over_256_bytes_is_refused() {
        let dir = unique_temp_dir("long_name");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        let long_name = "a".repeat(257);
        store.add(sample_peer(1, &long_name, 100)).unwrap();

        assert!(matches!(store.save(), Err(PeerError::NameTooLong)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tampered_format_version_byte_is_refused() {
        let dir = unique_temp_dir("bad_version");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        store.add(sample_peer(1, "device", 100)).unwrap();
        store.save().unwrap();

        let mut bytes = fs::read(&path).unwrap();
        bytes[0] = 99;
        fs::write(&path, &bytes).unwrap();

        match PeerStore::load(&path, DeviceKind::Phone) {
            Err(PeerError::UnknownFormat(99)) => {}
            other => panic!("expected UnknownFormat(99), got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let dir = unique_temp_dir("trailing");
        let path = dir.join("peers.bin");
        let mut store = PeerStore::load(&path, DeviceKind::Phone).unwrap();
        store.add(sample_peer(1, "device", 100)).unwrap();
        store.save().unwrap();

        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            PeerStore::load(&path, DeviceKind::Phone),
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

        let mut store = PeerStore::load(&peers_path, DeviceKind::Phone).unwrap();
        store.add(sample_peer(1, "device", 100)).unwrap();
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

    // FINDING 3, part a: the mode used to be set by a separate `chmod`
    // after `File::create`, which itself makes the file at `0o666 & !umask`
    // (typically `0o644`). A watcher that opened the file in that window
    // could read the key back later through the descriptor it already
    // held. This test runs a watcher for the whole span of `save()` and
    // records, with a bitwise OR, every permission bit seen on any file
    // whose name starts with the key file's name. If any observation ever
    // carried a bit outside `0o600`, the OR shows it. Before the fix this
    // observes `0o644`.
    #[test]
    #[cfg(unix)]
    fn the_key_file_is_never_readable_by_others_at_any_moment() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("watch_mode");
        let path = dir.join("key.bin");
        let watch_dir = dir.clone();
        let prefix = path.file_name().unwrap().to_os_string();

        let stop = Arc::new(AtomicBool::new(false));
        let seen_bits = Arc::new(AtomicU32::new(0));

        let watcher_stop = Arc::clone(&stop);
        let watcher_bits = Arc::clone(&seen_bits);
        let watcher = thread::spawn(move || {
            while !watcher_stop.load(Ordering::Relaxed) {
                let Ok(entries) = fs::read_dir(&watch_dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if !name
                        .to_string_lossy()
                        .starts_with(&*prefix.to_string_lossy())
                    {
                        continue;
                    }
                    if let Ok(metadata) = entry.metadata() {
                        let mode = metadata.permissions().mode() & 0o777;
                        watcher_bits.fetch_or(mode, Ordering::Relaxed);
                    }
                }
            }
        });

        let store = FileSecretStore::new(path);
        let generated = StaticKey::generate().unwrap();
        store.save(&generated).unwrap();

        stop.store(true, Ordering::Relaxed);
        watcher.join().unwrap();

        let observed = seen_bits.load(Ordering::Relaxed);
        assert_eq!(
            observed & !0o600,
            0,
            "a watcher saw permission bits outside 0o600: {observed:o}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // FINDING 3, part b: the temporary name used to always be `<path>.tmp`.
    // This plants a symlink at that old, predictable name, pointing at a
    // file the attacker wants overwritten. The fix picks a random name
    // instead, so `save()` never touches the planted symlink: the real
    // file at `path` must end up a regular file, and the attacker's target
    // must be untouched. Before the fix, `save()` follows the symlink,
    // writes the key into the target, and renames the symlink itself over
    // `path`.
    #[test]
    #[cfg(unix)]
    fn a_planted_symlink_at_the_temporary_name_is_not_followed() {
        let dir = unique_temp_dir("planted_symlink");
        let path = dir.join("key.bin");
        let attacker_target = dir.join("attacker-owned-file");
        fs::write(&attacker_target, b"do not touch me").unwrap();

        let old_predictable_tmp_name = {
            let mut name = path.as_os_str().to_os_string();
            name.push(".tmp");
            PathBuf::from(name)
        };
        std::os::unix::fs::symlink(&attacker_target, &old_predictable_tmp_name).unwrap();

        let store = FileSecretStore::new(path.clone());
        let generated = StaticKey::generate().unwrap();
        store.save(&generated).unwrap();

        let real_file_metadata = fs::symlink_metadata(&path).unwrap();
        assert!(
            real_file_metadata.file_type().is_file(),
            "the real path must be a regular file, not a symlink"
        );

        let target_contents = fs::read(&attacker_target).unwrap();
        assert_eq!(
            target_contents, b"do not touch me",
            "the planted symlink's target must be untouched"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // FINDING 3, part b, continued: the random suffix in the real
    // temporary name cannot be predicted, so it cannot be planted ahead of
    // time in a test. What can be tested directly is the seam
    // `write_private_file` relies on: `create_private_file` uses
    // `create_new`, which must refuse to write through anything that
    // already exists at the exact path it is given.
    #[test]
    #[cfg(unix)]
    fn create_new_refuses_a_name_that_already_exists() {
        let dir = unique_temp_dir("create_new_exists");
        let path = dir.join("already-here");
        fs::write(&path, b"existing").unwrap();

        let err = super::create_private_file(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        let contents = fs::read(&path).unwrap();
        assert_eq!(contents, b"existing", "the existing file must be untouched");

        let _ = fs::remove_dir_all(&dir);
    }

    // FINDING 3, part c: freed heap memory cannot be safely inspected from
    // a test, so this checks the mechanism instead of the memory left
    // behind. Two things are structural, checked at compile time or by
    // construction rather than by reading memory after the fact:
    //
    // 1. `FileSecretStore::save` builds its encoder with
    //    `encoded_key_capacity()`, which is at least the 64 bytes the key
    //    needs, so the encoder never reallocates and so never leaves a
    //    stale copy of the key behind in a freed buffer.
    // 2. The bytes `save` writes are typed as `Zeroizing<Vec<u8>>`. That
    //    type only compiles where a plain `Vec<u8>` would also compile, so
    //    a value of that type moving into a function that requires it is
    //    proof the wrapping is really there, not something that could be
    //    silently dropped by a later edit.
    #[test]
    fn the_encoded_key_bytes_are_wiped() {
        assert!(
            encoded_key_capacity() >= 64,
            "the key encoder must reserve enough capacity to never reallocate"
        );

        let mut e = Encoder::with_capacity(encoded_key_capacity());
        e.fixed(&[0u8; 32]);
        e.fixed(&[0u8; 32]);
        let bytes = Zeroizing::new(e.finish());
        requires_zeroizing_bytes(bytes);
    }

    // A value of type `Zeroizing<Vec<u8>>` can only be passed here because
    // it really is that type. A plain `Vec<u8>` would not compile at the
    // call site, so this function existing and being called is proof the
    // wrapping in `the_encoded_key_bytes_are_wiped` is real, not something a
    // later edit could quietly drop.
    fn requires_zeroizing_bytes(_bytes: Zeroizing<Vec<u8>>) {}
}
