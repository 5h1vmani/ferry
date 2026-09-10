//! Shared core for Ferry.
//!
//! Ferry moves files between macOS and Android. The core is split into two
//! layers, and this crate will hold both.
//!
//! # The file operations layer
//!
//! A small set of remote operations that either side can serve and either side
//! can call: `list`, `stat`, `read(path, offset, length)`, `write(path,
//! offset, bytes)`, `mkdir`, and `delete`.
//!
//! Reads and writes take a byte range. Finder must list files without
//! downloading them and fetch bytes on demand, so a whole-file read API cannot
//! work.
//!
//! # The transfer engine
//!
//! A transfer is a loop of `read` or `write` calls on top of the file
//! operations layer. Pushing is repeated `write`. Pulling is repeated `read`.
//! One protocol, used in two directions.
//!
//! # Current state
//!
//! Phase 1 has not started. This crate holds only the types that are already
//! decided. See `PLAN.md` and `docs/protocol.md`.

pub mod chunk;
pub mod frame;
pub mod limits;
/// The reference filesystem, used by tests and by nothing that ships.
#[cfg(any(test, feature = "testing"))]
pub mod memfs;
pub mod noise;
pub mod ops;
pub mod path;
pub mod rpc;
pub mod session;
pub mod transport;
pub mod version;
pub mod wire;

pub use chunk::{ChainingValue, ChunkSize, ChunkSizeError, Manifest, ManifestBuilder};
pub use frame::{Frame, FrameError, FrameKind, read_frame, write_frame};
#[cfg(any(test, feature = "testing"))]
pub use memfs::MemoryFs;
pub use ops::{Entry, FileKind, OpError, Request, Response};
pub use path::{PathError, RemotePath};
pub use rpc::{Client, FileOps, RpcError, serve};
pub use session::{Progress, SessionId, Transfer, TransferError, pull, resume_point};
pub use transport::{Endpoint, loopback};
pub use version::{Agreed, Role, VersionError, negotiate};
pub use wire::{Decoder, Encoder, WireError};
