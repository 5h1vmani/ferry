//! The file operations layer.
//!
//! Nine operations, defined in `docs/protocol.md` section 8. Either side of a
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

use std::fmt;

use crate::limits;
use crate::path::RemotePath;
use crate::wire::{Decoder, Encoder, WireError};

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

// Wire tags for `Response`. This is a separate space from the opcodes above.
// See the module documentation for why a response must decode without the
// request that produced it.
const RESPONSE_LIST: u8 = 1;
const RESPONSE_STAT: u8 = 2;
const RESPONSE_READ: u8 = 3;
const RESPONSE_WRITE: u8 = 4;
const RESPONSE_OK: u8 = 5;

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
    /// section 7 explains why version 1 has no recursive delete.
    Delete {
        /// The file or directory to delete.
        path: RemotePath,
    },
}

impl Request {
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
}

impl Response {
    /// Encode this response to its wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Self::List {
                entries,
                next_cursor,
            } => {
                e.u8(RESPONSE_LIST);
                // Matches the fallback `Encoder::bytes` already uses for an
                // over-long length: clamp rather than panic, since encoding
                // has no way to report failure.
                let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                e.u32(count);
                for entry in entries {
                    entry.encode(&mut e);
                }
                // `Encoder` has no built-in `Option`, so a one-byte presence
                // tag stands in for it, the same way every variant here uses
                // a tag byte to say what follows.
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
            Self::Stat { entry } => {
                e.u8(RESPONSE_STAT);
                entry.encode(&mut e);
            }
            Self::Read { bytes } => {
                e.u8(RESPONSE_READ);
                e.bytes(bytes);
            }
            Self::Write { written } => {
                e.u8(RESPONSE_WRITE);
                e.u32(*written);
            }
            Self::Ok => {
                e.u8(RESPONSE_OK);
            }
        }
        e.finish()
    }

    /// Decode a response from its wire form.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnknownTag`] for a tag this version does not
    /// know. Returns [`WireError::TooLong`] when a `list` page holds more
    /// than [`limits::MAX_LIST_ENTRIES`] entries, or when the read bytes are
    /// over [`limits::MAX_READ_LEN`]. Returns [`WireError::TrailingBytes`]
    /// when bytes are left over after a value this long has been read.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut d = Decoder::new(bytes);
        let tag = d.u8()?;
        let response = match tag {
            RESPONSE_LIST => {
                let count = d.u32()?;
                if count > limits::MAX_LIST_ENTRIES {
                    return Err(WireError::TooLong);
                }
                // `count` is now known to be at most `MAX_LIST_ENTRIES`,
                // which fits comfortably in a `usize` on every target Ferry
                // runs on.
                let mut entries = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    entries.push(Entry::decode(&mut d)?);
                }
                let next_cursor = match d.u8()? {
                    0 => None,
                    1 => Some(d.u64()?),
                    other => return Err(WireError::UnknownTag(other)),
                };
                Self::List {
                    entries,
                    next_cursor,
                }
            }
            RESPONSE_STAT => Self::Stat {
                entry: Entry::decode(&mut d)?,
            },
            RESPONSE_READ => {
                // A read response cannot rationally carry more bytes than the
                // largest read a request may ask for, so the same cap
                // applies here.
                let bytes = d.bytes(limits::MAX_READ_LEN as usize)?.to_vec();
                Self::Read { bytes }
            }
            RESPONSE_WRITE => Self::Write { written: d.u32()? },
            RESPONSE_OK => Self::Ok,
            other => return Err(WireError::UnknownTag(other)),
        };
        d.finish()?;
        Ok(response)
    }
}

/// The reason a file operation could not be carried out.
///
/// This is a fixed, structured value, never text. A caller acts on the tag
/// directly, instead of parsing a message meant for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpError {
    /// No file or directory exists at the path.
    NotFound = 1,
    /// The path names a file where a directory was required.
    NotADirectory = 2,
    /// The path names a directory where a file was required.
    IsADirectory = 3,
    /// A `delete` targeted a directory that still holds entries.
    NotEmpty = 4,
    /// The target of a `rename` or a `mkdir` already exists.
    AlreadyExists = 5,
    /// The filesystem refused the operation.
    PermissionDenied = 6,
    /// The path failed the checks in [`RemotePath::parse`].
    InvalidPath = 7,
    /// A `read` or `write` range was larger than the protocol allows.
    RangeTooLarge = 8,
    /// This side does not support the operation.
    Unsupported = 9,
    /// The operation failed for a reason the caller cannot act on.
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

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::NotFound => "no file or directory exists at the path",
            Self::NotADirectory => "the path is not a directory",
            Self::IsADirectory => "the path is a directory",
            Self::NotEmpty => "the directory is not empty",
            Self::AlreadyExists => "the target already exists",
            Self::PermissionDenied => "the filesystem denied permission",
            Self::InvalidPath => "the path failed validation",
            Self::RangeTooLarge => "the requested range is too large",
            Self::Unsupported => "this side does not support the operation",
            Self::Internal => "the operation failed for an internal reason",
        };
        f.write_str(text)
    }
}

impl std::error::Error for OpError {}

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
fn encode_i64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

fn decode_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::{Entry, FileKind, OpError, Request, Response, WireError};
    use crate::limits;
    use crate::path::RemotePath;

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
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
        ]
    }

    fn sample_responses() -> Vec<Response> {
        vec![
            Response::List {
                entries: vec![sample_entry()],
                next_cursor: Some(3),
            },
            Response::List {
                entries: Vec::new(),
                next_cursor: None,
            },
            Response::Stat {
                entry: sample_entry(),
            },
            Response::Read {
                bytes: b"file contents".to_vec(),
            },
            Response::Write { written: 5 },
            Response::Ok,
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
        for response in sample_responses() {
            let encoded = response.encode();
            assert_eq!(Response::decode(&encoded).unwrap(), response);
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
        assert_eq!(Response::decode(&encoded), Err(WireError::TooLong));
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
