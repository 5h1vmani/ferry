//! The devices this one has paired with, and where its own key lives.
//!
//! Not implemented yet. The contract below is the specification.
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
