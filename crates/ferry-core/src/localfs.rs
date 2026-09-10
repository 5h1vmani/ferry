//! The real filesystem, served through the file operations layer.
//!
//! Not implemented yet. The contract below is the specification.
//!
//! # Contract
//!
//! `LocalFs` implements [`crate::rpc::FileOps`] over one shared root on disk.
//! It is the only thing that turns a peer's path into a real file, so every
//! rule in `docs/protocol.md` section 8 is enforced here and nowhere else.
//!
//! It is built on `cap_std::fs::Dir`, which resolves paths inside a directory
//! capability and cannot be talked into leaving it. That replaces hand-written
//! `O_NOFOLLOW` handling with a primitive that already does it correctly.
//!
//! Rules:
//!
//! 1. A path that resolves outside the root is refused, including through a
//!    symlink. `cap_std` guarantees this. A test proves it with a real symlink.
//! 2. Only regular files and directories are served. A FIFO, a socket, a
//!    device, or a symlink that reaches one is refused with
//!    [`crate::ops::OpError::Unsupported`].
//! 3. `delete` is not recursive. A directory holding anything returns
//!    [`crate::ops::OpError::NotEmpty`].
//! 4. `rename` replaces the destination in one step.
//! 5. `read` past the end returns fewer bytes. `write` past the end extends
//!    with zeros. `truncate` shortens or extends.
//! 6. `list` pages at [`crate::limits::MAX_LIST_ENTRIES`], sorted by name, with
//!    the cursor as a zero-based index into the sorted children.
//! 7. Every std or cap_std error maps to one [`crate::ops::OpError`] variant.
//!    The mapping lives in one function.
//!
//! Public shape:
//!
//! ```text
//! pub struct LocalFs { .. }
//! impl LocalFs {
//!     /// Open a root. It must exist and be a directory.
//!     pub fn open(root: impl AsRef<std::path::Path>) -> Result<Self, OpError>;
//! }
//! impl FileOps for LocalFs { .. }
//! ```
