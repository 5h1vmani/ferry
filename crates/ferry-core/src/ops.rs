//! The file operations layer.
//!
//! Nine operations, defined in `docs/protocol.md` section 8, plus a manifest
//! request added by `docs/engine-contract.md` item 16a. Either side of a
//! connection can serve them, and either side can call them.
//!
//! [`Request`] and [`Response`] each start with their own tag byte. The two
//! tag spaces are independent, so a [`Response`] decodes on its own, without
//! first decoding the [`Request`] it answers. The request that caused a given
//! response is known from the frame's request identifier, not from replaying
//! the request bytes. This keeps the two enums free to change shape at
//! different rates.
//!
//! [`OpError`] carries no text. A caller matches on a fixed tag and decides
//! what to do, rather than parsing an English sentence.
//!
//! All three types reuse the [`Encoder`] and [`Decoder`] from [`crate::wire`].
//! Every decode enforces the caps in [`crate::limits`], so a peer cannot force
//! this side to allocate an unbounded amount of memory from a few header
//! bytes.

use crate::chunk::Manifest;
use crate::limits;
use crate::path::RemotePath;
use crate::wire::{Decoder, Encoder, WireError, decode_i64, encode_i64};

// Wire opcodes for `Request`. These numbers are the wire format: a variant
// keeps its number even if the enum's declaration order changes later.
const REQUEST_LIST: u8 = 1;
const REQUEST_STAT: u8 = 2;
const REQUEST_READ: u8 = 3;
const REQUEST_WRITE: u8 = 4;
const REQUEST_TRUNCATE: u8 = 5;
const REQUEST_RENAME: u8 = 6;
const REQUEST_SET_MTIME: u8 = 7;
const REQUEST_MKDIR: u8 = 8;
const REQUEST_DELETE: u8 = 9;
const REQUEST_MANIFEST: u8 = 10;

// Wire tags for `Response`. This is a separate space from the opcodes above.
// See the module documentation for why a response must decode without the
// request that produced it.

/// What kind of filesystem object an [`Entry`] names.
///
/// A symlink or a device file never becomes an [`Entry`]. The filesystem
/// layer refuses both before this type is built. See the module
/// documentation in `crate::path` for why a lexical path check cannot catch
/// them on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FileKind {
    /// A regular file.
    File = 1,
    /// A directory.
    Directory = 2,
}

impl FileKind {
    // Private: only `Entry::decode` needs this, so it carries no public
    // contract of its own.
    fn from_byte(value: u8) -> Result<Self, WireError> {
        match value {
            1 => Ok(Self::File),
            2 => Ok(Self::Directory),
            other => Err(WireError::UnknownTag(other)),
        }
    }
}

/// One line of a directory listing, or the answer to [`Request::Stat`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's name within its parent directory. Never a full path.
    ///
    /// The shared root has no parent to be named within, so a `stat` of the
    /// empty path (see `crate::path::RemotePath::is_root`) carries the empty
    /// string here instead.
    pub name: String,
    /// Whether the entry is a file or a directory.
    pub kind: FileKind,
    /// The size in bytes.
    pub size: u64,
    /// The last modification time, in seconds since the Unix epoch.
    pub modified_unix_secs: i64,
}

impl Entry {
    // Private: entries only ever travel inside a `Response`, so they do not
    // need their own public wire contract.
    fn encode(&self, e: &mut Encoder) {
        e.text(&self.name);
        e.u8(self.kind as u8);
        e.u64(self.size);
        e.u64(encode_i64(self.modified_unix_secs));
    }

    fn decode(d: &mut Decoder) -> Result<Self, WireError> {
        // A name is bounded the same way a path is. It is one component of a
        // path, so it can never be longer than a whole path.
        let name = d.text(limits::MAX_PATH_LEN)?.to_string();
        let kind = FileKind::from_byte(d.u8()?)?;
        let size = d.u64()?;
        let modified_unix_secs = decode_i64(d.u64()?);
        Ok(Self {
            name,
            kind,
            size,
            modified_unix_secs,
        })
    }
}

/// A call from one side of a connection to the other.
///
/// See `docs/protocol.md` section 8 for what each operation does. Every
/// variant carries a [`RemotePath`], which has already passed the checks in
/// [`RemotePath::parse`] by the time it reaches this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// List a page of a directory's entries, starting at `cursor`.
    List {
        /// The directory to list.
        path: RemotePath,
        /// Where to resume. Zero starts from the beginning.
        cursor: u64,
    },
    /// Read one entry's metadata.
    Stat {
        /// The file or directory to describe.
        path: RemotePath,
    },
    /// Read a byte range from a file.
    Read {
        /// The file to read.
        path: RemotePath,
        /// The byte offset to start at.
        offset: u64,
        /// How many bytes to read. Bounded by [`limits::MAX_READ_LEN`].
        length: u32,
    },
    /// Write a byte range into a file, extending it if the range reaches past
    /// the current end.
    Write {
        /// The file to write.
        path: RemotePath,
        /// The byte offset to start at.
        offset: u64,
        /// The bytes to write. Bounded by [`limits::MAX_WRITE_LEN`].
        bytes: Vec<u8>,
    },
    /// Shorten a file to `length` bytes.
    Truncate {
        /// The file to shorten.
        path: RemotePath,
        /// The new length, in bytes.
        length: u64,
    },
    /// Move or rename a file or directory.
    Rename {
        /// The current path.
        from: RemotePath,
        /// The path to move it to.
        to: RemotePath,
    },
    /// Set a file or directory's modification time.
    SetMtime {
        /// The file or directory to update.
        path: RemotePath,
        /// The new modification time, in seconds since the Unix epoch.
        modified_unix_secs: i64,
    },
    /// Create a directory.
    Mkdir {
        /// The directory to create.
        path: RemotePath,
    },
    /// Delete a file, or a directory that is empty.
    ///
    /// Deleting a directory that is not empty is refused. `docs/protocol.md`
    /// section 8 explains why version 2 has no recursive delete.
    Delete {
        /// The file or directory to delete.
        path: RemotePath,
    },
    /// Compute a file's manifest: its length, chunk size, chaining values,
    /// and root hash.
    ///
    /// Refused on a directory with [`OpError::IsADirectory`].
    /// `docs/engine-contract.md` item 16a.
    Manifest {
        /// The file to describe.
        path: RemotePath,
    },
}

impl Request {
    /// The opcode that names this operation on the wire.
    ///
    /// The reply to a request carries no tag of its own. Its shape follows
    /// from this opcode, so the mapping lives in one place instead of two.
    #[must_use]
    pub fn opcode(&self) -> u8 {
        match self {
            Self::List { .. } => REQUEST_LIST,
            Self::Stat { .. } => REQUEST_STAT,
            Self::Read { .. } => REQUEST_READ,
            Self::Write { .. } => REQUEST_WRITE,
            Self::Truncate { .. } => REQUEST_TRUNCATE,
            Self::Rename { .. } => REQUEST_RENAME,
            Self::SetMtime { .. } => REQUEST_SET_MTIME,
            Self::Mkdir { .. } => REQUEST_MKDIR,
            Self::Delete { .. } => REQUEST_DELETE,
            Self::Manifest { .. } => REQUEST_MANIFEST,
        }
    }

    /// Encode this request to its wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Self::List { path, cursor } => {
                e.u8(REQUEST_LIST);
                encode_path(&mut e, path);
                e.u64(*cursor);
            }
            Self::Stat { path } => {
                e.u8(REQUEST_STAT);
                encode_path(&mut e, path);
            }
            Self::Read {
                path,
                offset,
                length,
            } => {
                e.u8(REQUEST_READ);
                encode_path(&mut e, path);
                e.u64(*offset);
                e.u32(*length);
            }
            Self::Write {
                path,
                offset,
                bytes,
            } => {
                e.u8(REQUEST_WRITE);
                encode_path(&mut e, path);
                e.u64(*offset);
                e.bytes(bytes);
            }
            Self::Truncate { path, length } => {
                e.u8(REQUEST_TRUNCATE);
                encode_path(&mut e, path);
                e.u64(*length);
            }
            Self::Rename { from, to } => {
                e.u8(REQUEST_RENAME);
                encode_path(&mut e, from);
                encode_path(&mut e, to);
            }
            Self::SetMtime {
                path,
                modified_unix_secs,
            } => {
                e.u8(REQUEST_SET_MTIME);
                encode_path(&mut e, path);
                e.u64(encode_i64(*modified_unix_secs));
            }
            Self::Mkdir { path } => {
                e.u8(REQUEST_MKDIR);
                encode_path(&mut e, path);
            }
            Self::Delete { path } => {
                e.u8(REQUEST_DELETE);
                encode_path(&mut e, path);
            }
            Self::Manifest { path } => {
                e.u8(REQUEST_MANIFEST);
                encode_path(&mut e, path);
            }
        }
        e.finish()
    }

    /// Decode a request from its wire form.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnknownTag`] for an opcode this version does not
    /// know. Returns [`WireError::TooLong`] when a path, a `read` length, or a
    /// `write` payload is over its limit. Returns [`WireError::InvalidPath`]
    /// when a path fails the checks in [`RemotePath::parse`], such as holding
    /// a `..` component. Returns [`WireError::TrailingBytes`] when bytes are
    /// left over after a value this long has been read.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut d = Decoder::new(bytes);
        let opcode = d.u8()?;
        let request = match opcode {
            REQUEST_LIST => {
                let path = decode_path(&mut d)?;
                let cursor = d.u64()?;
                Self::List { path, cursor }
            }
            REQUEST_STAT => Self::Stat {
                path: decode_path(&mut d)?,
            },
            REQUEST_READ => {
                let path = decode_path(&mut d)?;
                let offset = d.u64()?;
                let length = d.u32()?;
                if length > limits::MAX_READ_LEN {
                    return Err(WireError::TooLong);
                }
                Self::Read {
                    path,
                    offset,
                    length,
                }
            }
            REQUEST_WRITE => {
                let path = decode_path(&mut d)?;
                let offset = d.u64()?;
                // `Decoder::bytes` checks the limit itself before it copies
                // anything out, so a lie about the length costs nothing.
                let bytes = d.bytes(limits::MAX_WRITE_LEN as usize)?.to_vec();
                Self::Write {
                    path,
                    offset,
                    bytes,
                }
            }
            REQUEST_TRUNCATE => {
                let path = decode_path(&mut d)?;
                let length = d.u64()?;
                Self::Truncate { path, length }
            }
            REQUEST_RENAME => {
                let from = decode_path(&mut d)?;
                let to = decode_path(&mut d)?;
                Self::Rename { from, to }
            }
            REQUEST_SET_MTIME => {
                let path = decode_path(&mut d)?;
                let modified_unix_secs = decode_i64(d.u64()?);
                Self::SetMtime {
                    path,
                    modified_unix_secs,
                }
            }
            REQUEST_MKDIR => Self::Mkdir {
                path: decode_path(&mut d)?,
            },
            REQUEST_DELETE => Self::Delete {
                path: decode_path(&mut d)?,
            },
            REQUEST_MANIFEST => Self::Manifest {
                path: decode_path(&mut d)?,
            },
            other => return Err(WireError::UnknownTag(other)),
        };
        d.finish()?;
        Ok(request)
    }
}

/// A successful answer to a [`Request`] with the same identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// A page of a directory's entries, answering [`Request::List`].
    List {
        /// The entries in this page, in server order.
        entries: Vec<Entry>,
        /// The cursor for the next page, or `None` when this was the last
        /// page.
        next_cursor: Option<u64>,
    },
    /// One entry's metadata, answering [`Request::Stat`].
    Stat {
        /// The entry that was described.
        entry: Entry,
    },
    /// The bytes read, answering [`Request::Read`].
    Read {
        /// The bytes read from the file.
        bytes: Vec<u8>,
    },
    /// How many bytes were written, answering [`Request::Write`].
    Write {
        /// The number of bytes written.
        written: u32,
    },
    /// An answer with no data, for a request that only succeeds or fails.
    ///
    /// Answers [`Request::Truncate`], [`Request::Rename`],
    /// [`Request::SetMtime`], [`Request::Mkdir`], and [`Request::Delete`].
    Ok,
    /// A file's manifest, answering [`Request::Manifest`].
    Manifest {
        /// The manifest, encoded with [`Manifest::encode`].
        manifest: Manifest,
    },
}

impl Response {
    /// Encode this response to its wire form.
    ///
    /// The bytes carry no tag. The opcode of the request already says what
    /// shape the reply has, so repeating it would let the two disagree.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Self::List {
                entries,
                next_cursor,
            } => {
                // Matches the fallback `Encoder::bytes` already uses for an
                // over-long length: clamp rather than panic, since encoding
                // has no way to report failure.
                e.u32(u32::try_from(entries.len()).unwrap_or(u32::MAX));
                for entry in entries {
                    entry.encode(&mut e);
                }
                // `Encoder` has no built-in `Option`, so a one-byte presence
                // tag stands in for it.
                match next_cursor {
                    Some(cursor) => {
                        e.u8(1);
                        e.u64(*cursor);
                    }
                    None => {
                        e.u8(0);
                    }
                }
            }
            Self::Stat { entry } => entry.encode(&mut e),
            Self::Read { bytes } => {
                e.bytes(bytes);
            }
            Self::Write { written } => {
                e.u32(*written);
            }
            Self::Ok => {}
            Self::Manifest { manifest } => {
                e.bytes(&manifest.encode());
            }
        }
        e.finish()
    }

    /// Decode the reply to a `list`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TooLong`] when the page holds more than
    /// [`limits::MAX_LIST_ENTRIES`] entries, and [`WireError::InvalidPath`]
    /// when an entry's name is empty or holds a `/`. Neither name is a real
    /// child of the folder listed: the empty name is only ever valid on the
    /// single entry [`Response::decode_stat`] returns for the shared root
    /// itself, never inside a listing, and a `/` would let one entry name a
    /// path several components deep.
    pub fn decode_list(bytes: &[u8]) -> Result<(Vec<Entry>, Option<u64>), WireError> {
        let mut d = Decoder::new(bytes);
        let count = d.u32()?;
        if count > limits::MAX_LIST_ENTRIES {
            return Err(WireError::TooLong);
        }
        // The count is capped before anything is reserved.
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let entry = Entry::decode(&mut d)?;
            if entry.name.is_empty() || entry.name.contains('/') {
                return Err(WireError::InvalidPath);
            }
            entries.push(entry);
        }
        let next_cursor = match d.u8()? {
            0 => None,
            1 => Some(d.u64()?),
            other => return Err(WireError::UnknownTag(other)),
        };
        d.finish()?;
        Ok((entries, next_cursor))
    }

    /// Decode the reply to a `stat`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when the entry is incomplete.
    pub fn decode_stat(bytes: &[u8]) -> Result<Entry, WireError> {
        let mut d = Decoder::new(bytes);
        let entry = Entry::decode(&mut d)?;
        d.finish()?;
        Ok(entry)
    }

    /// Decode the reply to a `read`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TooLong`] when the reply carries more than
    /// [`limits::MAX_READ_LEN`] bytes, which no honest reply ever does.
    pub fn decode_read(bytes: &[u8]) -> Result<Vec<u8>, WireError> {
        let mut d = Decoder::new(bytes);
        let out = d.bytes(limits::MAX_READ_LEN as usize)?.to_vec();
        d.finish()?;
        Ok(out)
    }

    /// Decode the reply to a `write`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnexpectedEnd`] when the count is missing.
    pub fn decode_write(bytes: &[u8]) -> Result<u32, WireError> {
        let mut d = Decoder::new(bytes);
        let written = d.u32()?;
        d.finish()?;
        Ok(written)
    }

    /// Decode the reply to an operation that carries no data.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TrailingBytes`] when anything at all is present.
    /// An empty reply is the only correct answer, so bytes here mean the two
    /// sides disagree about the format.
    pub fn decode_ok(bytes: &[u8]) -> Result<(), WireError> {
        Decoder::new(bytes).finish()
    }

    /// Decode the reply to a `manifest` request.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TooLong`] when the manifest is over
    /// [`limits::MAX_MANIFEST_BYTES`], and [`WireError::BadManifest`] when
    /// the bytes do not decode as a manifest that agrees with itself.
    pub fn decode_manifest(bytes: &[u8]) -> Result<Manifest, WireError> {
        let mut d = Decoder::new(bytes);
        let manifest_bytes = d.bytes(limits::MAX_MANIFEST_BYTES)?;
        let manifest = Manifest::decode(manifest_bytes).map_err(|_| WireError::BadManifest)?;
        d.finish()?;
        Ok(manifest)
    }

    /// Decode a reply, given the opcode of the request it answers.
    ///
    /// The opcode decides the shape. A reply that does not fit that shape
    /// fails to decode, so a peer cannot answer one question with another.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnknownTag`] when the opcode names no operation
    /// this version knows, and the errors of the shape-specific decoders.
    pub fn decode(opcode: u8, bytes: &[u8]) -> Result<Self, WireError> {
        match opcode {
            REQUEST_LIST => {
                let (entries, next_cursor) = Self::decode_list(bytes)?;
                Ok(Self::List {
                    entries,
                    next_cursor,
                })
            }
            REQUEST_STAT => Ok(Self::Stat {
                entry: Self::decode_stat(bytes)?,
            }),
            REQUEST_READ => Ok(Self::Read {
                bytes: Self::decode_read(bytes)?,
            }),
            REQUEST_WRITE => Ok(Self::Write {
                written: Self::decode_write(bytes)?,
            }),
            REQUEST_TRUNCATE | REQUEST_RENAME | REQUEST_SET_MTIME | REQUEST_MKDIR
            | REQUEST_DELETE => {
                Self::decode_ok(bytes)?;
                Ok(Self::Ok)
            }
            REQUEST_MANIFEST => Ok(Self::Manifest {
                manifest: Self::decode_manifest(bytes)?,
            }),
            other => Err(WireError::UnknownTag(other)),
        }
    }
}

/// The reason a file operation could not be carried out.
///
/// This is a fixed, structured value, never text. A caller acts on the tag
/// directly, instead of parsing a message meant for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[repr(u8)]
pub enum OpError {
    /// No file or directory exists at the path.
    #[error("no file or directory exists at the path")]
    NotFound = 1,
    /// The path names a file where a directory was required.
    #[error("the path is not a directory")]
    NotADirectory = 2,
    /// The path names a directory where a file was required.
    #[error("the path is a directory")]
    IsADirectory = 3,
    /// A `delete` targeted a directory that still holds entries.
    #[error("the directory is not empty")]
    NotEmpty = 4,
    /// The target of a `rename` or a `mkdir` already exists.
    #[error("the target already exists")]
    AlreadyExists = 5,
    /// The filesystem refused the operation.
    #[error("the filesystem denied permission")]
    PermissionDenied = 6,
    /// The path failed the checks in [`RemotePath::parse`].
    #[error("the path failed validation")]
    InvalidPath = 7,
    /// A `read` or `write` range was larger than the protocol allows.
    #[error("the requested range is too large")]
    RangeTooLarge = 8,
    /// This side does not support the operation.
    #[error("this side does not support the operation")]
    Unsupported = 9,
    /// The operation failed for a reason the caller cannot act on.
    #[error("the operation failed for an internal reason")]
    Internal = 10,
}

impl OpError {
    // Private: only `OpError::decode` needs this.
    fn from_byte(value: u8) -> Result<Self, WireError> {
        match value {
            1 => Ok(Self::NotFound),
            2 => Ok(Self::NotADirectory),
            3 => Ok(Self::IsADirectory),
            4 => Ok(Self::NotEmpty),
            5 => Ok(Self::AlreadyExists),
            6 => Ok(Self::PermissionDenied),
            7 => Ok(Self::InvalidPath),
            8 => Ok(Self::RangeTooLarge),
            9 => Ok(Self::Unsupported),
            10 => Ok(Self::Internal),
            other => Err(WireError::UnknownTag(other)),
        }
    }

    /// Encode this error to its wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.u8(*self as u8);
        e.finish()
    }

    /// Decode an error from its wire form.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnknownTag`] for a tag this version does not
    /// know, and [`WireError::TrailingBytes`] when bytes are left over.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut d = Decoder::new(bytes);
        let error = Self::from_byte(d.u8()?)?;
        d.finish()?;
        Ok(error)
    }
}

// Writes a path the same way any other text is written. This exists only so
// every `Request` variant does not repeat `path.as_str()` by hand.
fn encode_path(e: &mut Encoder, path: &RemotePath) {
    e.text(path.as_str());
}

// Reads text and re-validates it as a path. A length problem is caught by
// `Decoder::text` itself; anything else `RemotePath::parse` rejects, such as
// a `..` component, becomes `WireError::InvalidPath`.
fn decode_path(d: &mut Decoder) -> Result<RemotePath, WireError> {
    let text = d.text(limits::MAX_PATH_LEN)?;
    RemotePath::parse(text).map_err(|_| WireError::InvalidPath)
}

// `Encoder` and `Decoder` have no signed-integer methods. An `as` cast from
// `i64` to `u64` would be a truncating cast in clippy's eyes even though no
// bits are lost, so the bits are reinterpreted explicitly instead. This keeps
// negative values exact.
#[cfg(test)]
mod tests {
    use super::{Entry, FileKind, OpError, Request, Response, WireError};
    use crate::chunk::{ChunkSize, manifest_from_bytes};
    use crate::limits;
    use crate::path::RemotePath;

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    fn sample_manifest() -> super::Manifest {
        manifest_from_bytes(
            b"a manifest request's own answer",
            ChunkSize::one_mebibyte(),
        )
    }

    fn sample_entry() -> Entry {
        Entry {
            name: "IMG_0001.jpg".to_string(),
            kind: FileKind::File,
            size: 12_345,
            modified_unix_secs: -100,
        }
    }

    fn sample_requests() -> Vec<Request> {
        vec![
            Request::List {
                path: path("DCIM/Camera"),
                cursor: 7,
            },
            Request::Stat {
                path: path("DCIM/Camera/a.jpg"),
            },
            Request::Read {
                path: path("DCIM/Camera/a.jpg"),
                offset: 4096,
                length: 1024,
            },
            Request::Write {
                path: path("DCIM/Camera/a.jpg"),
                offset: 0,
                bytes: b"hello".to_vec(),
            },
            Request::Truncate {
                path: path("DCIM/Camera/a.jpg"),
                length: 0,
            },
            Request::Rename {
                from: path("DCIM/Camera/a.jpg"),
                to: path("DCIM/Camera/b.jpg"),
            },
            Request::SetMtime {
                path: path("DCIM/Camera/a.jpg"),
                modified_unix_secs: -1,
            },
            Request::Mkdir {
                path: path("DCIM/NewFolder"),
            },
            Request::Delete {
                path: path("DCIM/Camera/a.jpg"),
            },
            Request::Manifest {
                path: path("DCIM/Camera/a.jpg"),
            },
        ]
    }

    fn sample_responses() -> Vec<(Request, Response)> {
        vec![
            (
                Request::List {
                    path: path("DCIM/Camera"),
                    cursor: 0,
                },
                Response::List {
                    entries: vec![sample_entry()],
                    next_cursor: Some(3),
                },
            ),
            (
                Request::List {
                    path: path("DCIM/Camera"),
                    cursor: 0,
                },
                Response::List {
                    entries: Vec::new(),
                    next_cursor: None,
                },
            ),
            (
                Request::Stat {
                    path: path("DCIM/Camera"),
                },
                Response::Stat {
                    entry: sample_entry(),
                },
            ),
            (
                Request::Read {
                    path: path("DCIM/Camera"),
                    offset: 0,
                    length: 16,
                },
                Response::Read {
                    bytes: b"file contents".to_vec(),
                },
            ),
            (
                Request::Write {
                    path: path("DCIM/Camera"),
                    offset: 0,
                    bytes: Vec::new(),
                },
                Response::Write { written: 5 },
            ),
            (
                Request::Mkdir {
                    path: path("DCIM/Camera"),
                },
                Response::Ok,
            ),
            (
                Request::Manifest {
                    path: path("DCIM/Camera/a.jpg"),
                },
                Response::Manifest {
                    manifest: sample_manifest(),
                },
            ),
        ]
    }

    fn sample_errors() -> Vec<OpError> {
        vec![
            OpError::NotFound,
            OpError::NotADirectory,
            OpError::IsADirectory,
            OpError::NotEmpty,
            OpError::AlreadyExists,
            OpError::PermissionDenied,
            OpError::InvalidPath,
            OpError::RangeTooLarge,
            OpError::Unsupported,
            OpError::Internal,
        ]
    }

    #[test]
    fn every_request_variant_survives_a_round_trip() {
        for request in sample_requests() {
            let encoded = request.encode();
            assert_eq!(Request::decode(&encoded).unwrap(), request);
        }
    }

    #[test]
    fn every_response_variant_survives_a_round_trip() {
        for (request, response) in sample_responses() {
            let encoded = response.encode();
            let decoded = Response::decode(request.opcode(), &encoded).unwrap();
            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn every_op_error_variant_survives_a_round_trip() {
        for error in sample_errors() {
            let encoded = error.encode();
            assert_eq!(OpError::decode(&encoded).unwrap(), error);
        }
    }

    #[test]
    fn a_read_length_over_the_limit_is_refused_on_decode() {
        let request = Request::Read {
            path: path("a.jpg"),
            offset: 0,
            length: limits::MAX_READ_LEN + 1,
        };
        let encoded = request.encode();
        assert_eq!(Request::decode(&encoded), Err(WireError::TooLong));
    }

    #[test]
    fn a_write_payload_over_the_limit_is_refused_on_decode() {
        let request = Request::Write {
            path: path("a.jpg"),
            offset: 0,
            bytes: vec![0u8; (limits::MAX_WRITE_LEN + 1) as usize],
        };
        let encoded = request.encode();
        assert_eq!(Request::decode(&encoded), Err(WireError::TooLong));
    }

    #[test]
    fn a_list_response_over_the_entry_limit_is_refused_on_decode() {
        let too_many = (limits::MAX_LIST_ENTRIES + 1) as usize;
        let response = Response::List {
            entries: vec![sample_entry(); too_many],
            next_cursor: None,
        };
        let encoded = response.encode();
        let opcode = Request::List {
            path: path("DCIM/Camera"),
            cursor: 0,
        }
        .opcode();
        assert_eq!(Response::decode(opcode, &encoded), Err(WireError::TooLong));
    }

    #[test]
    fn a_list_entry_with_an_empty_name_is_refused_on_decode() {
        let response = Response::List {
            entries: vec![Entry {
                name: String::new(),
                ..sample_entry()
            }],
            next_cursor: None,
        };
        let encoded = response.encode();
        let opcode = Request::List {
            path: path("DCIM/Camera"),
            cursor: 0,
        }
        .opcode();
        assert_eq!(
            Response::decode(opcode, &encoded),
            Err(WireError::InvalidPath)
        );
    }

    #[test]
    fn a_list_entry_with_a_slash_in_its_name_is_refused_on_decode() {
        let response = Response::List {
            entries: vec![Entry {
                name: "DCIM/Camera".to_owned(),
                ..sample_entry()
            }],
            next_cursor: None,
        };
        let encoded = response.encode();
        let opcode = Request::List {
            path: path("DCIM/Camera"),
            cursor: 0,
        }
        .opcode();
        assert_eq!(
            Response::decode(opcode, &encoded),
            Err(WireError::InvalidPath)
        );
    }

    #[test]
    fn a_path_with_a_parent_component_is_refused_on_decode() {
        // `Request::encode` can only ever emit an already-validated path, so
        // the bad path is built by hand at the wire level instead.
        let mut e = super::Encoder::new();
        e.u8(super::REQUEST_STAT);
        e.text("../etc/passwd");
        let encoded = e.finish();
        assert_eq!(Request::decode(&encoded), Err(WireError::InvalidPath));
    }

    #[test]
    fn trailing_bytes_after_a_valid_request_are_refused() {
        let request = Request::Mkdir {
            path: path("DCIM/NewFolder"),
        };
        let mut encoded = request.encode();
        encoded.push(0);
        assert_eq!(Request::decode(&encoded), Err(WireError::TrailingBytes));
    }

    #[test]
    fn an_unknown_opcode_is_refused() {
        let mut e = super::Encoder::new();
        e.u8(200);
        let encoded = e.finish();
        assert_eq!(Request::decode(&encoded), Err(WireError::UnknownTag(200)));
    }
}
