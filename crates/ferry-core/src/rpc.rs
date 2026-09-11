//! Calling file operations across a connection, and serving them.
//!
//! [`FileOps`] is what a device offers. [`Client`] calls it across a stream.
//! [`serve`] answers those calls.
//!
//! Both sides call [`exchange_hello`] once, right after the Noise handshake
//! finishes and before `serve` or [`Client`] touch the stream. It carries
//! each side's display name across the encrypted channel, since the mDNS
//! name is random and an adb serial is a number, and neither is a name. It
//! also carries each side's [`DeviceKind`], one byte after the name.
//!
//! The protocol is symmetric, so either device can hold either role. One
//! connection currently carries one client and one server. Running both
//! directions at once needs multiplexing, which is not built yet.
//!
//! Requests are also answered one at a time. Pipelining needs the credit window
//! described in `docs/protocol.md` section 7, and that is not built yet either.
//! The frame format already carries request identifiers, so adding it later
//! does not change the wire format.

use std::io::{self, Read, Write};

use crate::chunk::Manifest;
use crate::frame::{Frame, FrameError, FrameKind, read_frame, write_frame};
use crate::ops::{Entry, OpError, Request, Response};
use crate::path::RemotePath;
use crate::peers::DeviceKind;
use crate::wire::{Decoder, Encoder, WireError};

/// The file operations one device offers to the other.
///
/// Methods take `&self` so that one implementation can be shared by several
/// connections. Anything that needs to change state uses interior mutability.
///
/// An implementation is responsible for the safety rules in
/// `docs/protocol.md` section 8. It resolves paths inside its own shared root,
/// refuses symlinks and special files, and never follows a path out of that
/// root.
pub trait FileOps: Send + Sync {
    /// List one page of a directory.
    ///
    /// Returns the entries and, when more remain, the cursor for the next page.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`] when the path does not exist, and
    /// [`OpError::NotADirectory`] when it is a file.
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError>;

    /// Describe one file or directory.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`] when the path does not exist.
    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError>;

    /// Read a byte range.
    ///
    /// A short result means the range ran past the end of the file.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`], [`OpError::IsADirectory`], or
    /// [`OpError::RangeTooLarge`] when the length is over the limit.
    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError>;

    /// Write a byte range, creating the file when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::IsADirectory`] or [`OpError::PermissionDenied`].
    fn write(&self, path: &RemotePath, offset: u64, bytes: &[u8]) -> Result<u32, OpError>;

    /// Set a file's length, cutting it short or extending it with zeros.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`] or [`OpError::IsADirectory`].
    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError>;

    /// Move a file or directory to another name.
    ///
    /// This is how a received file moves from its temporary name to its real
    /// one, so it must replace the destination in one step.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`] when the source is missing.
    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError>;

    /// Set a file's modified time, so a copied photo keeps its original date.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`].
    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError>;

    /// Create a directory.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::AlreadyExists`] or [`OpError::NotFound`] when a
    /// parent is missing.
    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError>;

    /// Delete a file, or an empty directory.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotEmpty`] for a directory holding anything. Delete
    /// is not recursive in version 2.
    fn delete(&self, path: &RemotePath) -> Result<(), OpError>;

    /// Compute a file's manifest: its length, chunk size, chaining values,
    /// and root hash.
    ///
    /// This reveals only what a `stat` already reveals and nothing more, so
    /// an implementation logs it as `Stat`. `docs/engine-contract.md` item
    /// 16a.
    ///
    /// # Errors
    ///
    /// Returns [`OpError::NotFound`] when the path does not exist, and
    /// [`OpError::IsADirectory`] when it names a directory.
    fn manifest(&self, path: &RemotePath) -> Result<Manifest, OpError>;
}

/// The reason a call failed.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The frame layer failed.
    #[error("frame layer failed: {0}")]
    Frame(#[from] FrameError),
    /// A payload did not decode.
    #[error("payload did not decode: {0}")]
    Wire(#[from] WireError),
    /// The peer refused the operation. This is a normal outcome, not a fault.
    #[error("the peer refused: {0}")]
    Remote(#[from] OpError),
    /// The peer used a request identifier that was never sent.
    #[error("expected request {expected} but got {got}")]
    MismatchedRequestId {
        /// What was sent.
        expected: u32,
        /// What came back.
        got: u32,
    },
    /// The peer sent a response where a request belonged, or the reverse.
    ///
    /// Also returned by [`exchange_hello`] when the first frame after the
    /// handshake is not a hello, including when the peer sends nothing that
    /// looks like one at all.
    #[error("unexpected frame kind {0:?}")]
    UnexpectedFrameKind(FrameKind),
    /// A name was empty, over [`MAX_NAME_LEN`] bytes, or held a control
    /// character.
    #[error("name failed validation")]
    BadName,
}

/// The longest a display name may be, in bytes.
///
/// A name is checked against this on both send and receive in
/// [`exchange_hello`]. It is also the limit passed to [`Decoder::text`] when
/// reading the peer's name, so an oversized name is refused during decoding
/// rather than after.
pub const MAX_NAME_LEN: usize = 64;

/// Exchange display names and device kinds with the peer.
///
/// Both sides call this once, immediately after the Noise handshake
/// finishes, and before `serve` or [`Client`] touch the stream. Each side
/// writes its own hello first, then reads the peer's, so neither side waits
/// on the other before sending its own.
///
/// The name is shown to the person on the other device and is never trusted
/// for anything. Identity is proven by the handshake's static keys, not by
/// this exchange. See `docs/protocol.md` section 5. The kind is shown
/// alongside the name; it is likewise never trusted as a security boundary.
///
/// # Errors
///
/// Returns [`RpcError::BadName`] when `my_name`, or the name the peer sends
/// back, is empty, over [`MAX_NAME_LEN`] bytes, or holds a control
/// character. Returns [`RpcError::UnexpectedFrameKind`] when the first frame
/// from the peer is not a hello with a request identifier of 0. Returns
/// [`RpcError::Wire`] when the peer's kind byte names no [`DeviceKind`].
pub fn exchange_hello(
    stream: &mut (impl Read + Write),
    my_name: &str,
    my_kind: DeviceKind,
) -> Result<(String, DeviceKind), RpcError> {
    let payload = encode_hello_payload(my_name, my_kind)?;
    write_frame(
        stream,
        &Frame {
            kind: FrameKind::Hello,
            request_id: 0,
            payload,
        },
    )?;

    let frame = read_frame(stream)?;
    if frame.kind != FrameKind::Hello || frame.request_id != 0 {
        return Err(RpcError::UnexpectedFrameKind(frame.kind));
    }
    decode_hello_payload(&frame.payload)
}

/// Encode a display name and a device kind the same way [`exchange_hello`]
/// puts them in a hello frame's payload: a length-prefixed piece of text,
/// then one byte for the kind.
///
/// `crate::noise`'s `IK` pairing handshake uses this directly, so the
/// initiator's hello can travel inside the handshake's first message instead
/// of needing a second hello once the channel opens. See
/// `docs/engine-contract.md` item 12.
///
/// # Errors
///
/// Returns [`RpcError::BadName`] under the same rule [`validate_name`]
/// checks.
pub(crate) fn encode_hello_payload(name: &str, kind: DeviceKind) -> Result<Vec<u8>, RpcError> {
    validate_name(name)?;
    let mut encoder = Encoder::new();
    encoder.text(name);
    encoder.u8(kind.to_byte());
    Ok(encoder.finish())
}

/// Decode what [`encode_hello_payload`] built.
///
/// # Errors
///
/// Returns [`RpcError::Wire`] when the bytes do not decode, and
/// [`RpcError::BadName`] when the name they carry fails validation.
pub(crate) fn decode_hello_payload(bytes: &[u8]) -> Result<(String, DeviceKind), RpcError> {
    let mut decoder = Decoder::new(bytes);
    let name = decoder.text(MAX_NAME_LEN)?.to_string();
    let kind = DeviceKind::from_byte(decoder.u8()?)?;
    decoder.finish()?;
    validate_name(&name)?;
    Ok((name, kind))
}

/// A name is 1 to 64 bytes of UTF-8 and holds no control character, meaning
/// anything below `U+0020` or `U+007F`. Those are unprintable and have no
/// place in a name someone reads on a screen.
fn validate_name(name: &str) -> Result<(), RpcError> {
    let right_length = !name.is_empty() && name.len() <= MAX_NAME_LEN;
    let no_control = !name.chars().any(|c| c.is_ascii_control());
    if right_length && no_control {
        Ok(())
    } else {
        Err(RpcError::BadName)
    }
}

/// Calls file operations on the other device.
#[derive(Debug)]
pub struct Client<S> {
    stream: S,
    next_id: u32,
}

impl<S: Read + Write> Client<S> {
    /// Wrap a connected stream.
    ///
    /// The stream is normally a `SecureStream`, but any byte stream works.
    pub fn new(stream: S) -> Self {
        Self { stream, next_id: 1 }
    }

    /// Give the stream back.
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Send one request and return the raw payload of its answer.
    ///
    /// The caller decodes that payload with the decoder for the shape it
    /// asked for. A reply that does not fit fails to decode, so a peer cannot
    /// answer one question with another.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::Remote`] when the peer refused the operation, which
    /// is an ordinary outcome.
    fn call_payload(&mut self, request: &Request) -> Result<Vec<u8>, RpcError> {
        let request_id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        write_frame(
            &mut self.stream,
            &Frame {
                kind: FrameKind::Request,
                request_id,
                payload: request.encode(),
            },
        )?;

        let frame = read_frame(&mut self.stream)?;
        if frame.request_id != request_id {
            return Err(RpcError::MismatchedRequestId {
                expected: request_id,
                got: frame.request_id,
            });
        }
        match frame.kind {
            FrameKind::Response => Ok(frame.payload),
            FrameKind::Error => Err(RpcError::Remote(OpError::decode(&frame.payload)?)),
            // A request or a hello can never be a sensible answer to a call.
            FrameKind::Request | FrameKind::Hello => Err(RpcError::UnexpectedFrameKind(frame.kind)),
        }
    }

    /// Send one request and wait for its answer.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::Remote`] when the peer refused the operation, and
    /// [`RpcError::Wire`] when the reply does not fit the shape the request
    /// asked for.
    pub fn call(&mut self, request: &Request) -> Result<Response, RpcError> {
        let payload = self.call_payload(request)?;
        Ok(Response::decode(request.opcode(), &payload)?)
    }

    /// List one page of a directory.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn list(
        &mut self,
        path: &RemotePath,
        cursor: u64,
    ) -> Result<(Vec<Entry>, Option<u64>), RpcError> {
        let payload = self.call_payload(&Request::List {
            path: path.clone(),
            cursor,
        })?;
        Ok(Response::decode_list(&payload)?)
    }

    /// Describe one file or directory.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn stat(&mut self, path: &RemotePath) -> Result<Entry, RpcError> {
        let payload = self.call_payload(&Request::Stat { path: path.clone() })?;
        Ok(Response::decode_stat(&payload)?)
    }

    /// Read a byte range.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn read(
        &mut self,
        path: &RemotePath,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, RpcError> {
        let payload = self.call_payload(&Request::Read {
            path: path.clone(),
            offset,
            length,
        })?;
        Ok(Response::decode_read(&payload)?)
    }

    /// Write a byte range.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn write(
        &mut self,
        path: &RemotePath,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<u32, RpcError> {
        let payload = self.call_payload(&Request::Write {
            path: path.clone(),
            offset,
            bytes,
        })?;
        Ok(Response::decode_write(&payload)?)
    }

    /// Set a file's length.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn truncate(&mut self, path: &RemotePath, length: u64) -> Result<(), RpcError> {
        let payload = self.call_payload(&Request::Truncate {
            path: path.clone(),
            length,
        })?;
        Ok(Response::decode_ok(&payload)?)
    }

    /// Move a file or directory.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn rename(&mut self, from: &RemotePath, to: &RemotePath) -> Result<(), RpcError> {
        let payload = self.call_payload(&Request::Rename {
            from: from.clone(),
            to: to.clone(),
        })?;
        Ok(Response::decode_ok(&payload)?)
    }

    /// Set a file's modified time.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn set_mtime(
        &mut self,
        path: &RemotePath,
        modified_unix_secs: i64,
    ) -> Result<(), RpcError> {
        let payload = self.call_payload(&Request::SetMtime {
            path: path.clone(),
            modified_unix_secs,
        })?;
        Ok(Response::decode_ok(&payload)?)
    }

    /// Create a directory.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn mkdir(&mut self, path: &RemotePath) -> Result<(), RpcError> {
        let payload = self.call_payload(&Request::Mkdir { path: path.clone() })?;
        Ok(Response::decode_ok(&payload)?)
    }

    /// Delete a file or an empty directory.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn delete(&mut self, path: &RemotePath) -> Result<(), RpcError> {
        let payload = self.call_payload(&Request::Delete { path: path.clone() })?;
        Ok(Response::decode_ok(&payload)?)
    }

    /// Compute a file's manifest.
    ///
    /// # Errors
    ///
    /// As [`Client::call`].
    pub fn manifest(&mut self, path: &RemotePath) -> Result<Manifest, RpcError> {
        let payload = self.call_payload(&Request::Manifest { path: path.clone() })?;
        Ok(Response::decode_manifest(&payload)?)
    }
}

fn handle(ops: &dyn FileOps, request: &Request) -> Result<Response, OpError> {
    match request {
        Request::List { path, cursor } => {
            let (entries, next_cursor) = ops.list(path, *cursor)?;
            Ok(Response::List {
                entries,
                next_cursor,
            })
        }
        Request::Stat { path } => Ok(Response::Stat {
            entry: ops.stat(path)?,
        }),
        Request::Read {
            path,
            offset,
            length,
        } => Ok(Response::Read {
            bytes: ops.read(path, *offset, *length)?,
        }),
        Request::Write {
            path,
            offset,
            bytes,
        } => Ok(Response::Write {
            written: ops.write(path, *offset, bytes)?,
        }),
        Request::Truncate { path, length } => ops.truncate(path, *length).map(|()| Response::Ok),
        Request::Rename { from, to } => ops.rename(from, to).map(|()| Response::Ok),
        Request::SetMtime {
            path,
            modified_unix_secs,
        } => ops
            .set_mtime(path, *modified_unix_secs)
            .map(|()| Response::Ok),
        Request::Mkdir { path } => ops.mkdir(path).map(|()| Response::Ok),
        Request::Delete { path } => ops.delete(path).map(|()| Response::Ok),
        Request::Manifest { path } => Ok(Response::Manifest {
            manifest: ops.manifest(path)?,
        }),
    }
}

/// Answer requests until the peer stops or the connection fails.
///
/// Returns normally when the peer closes the connection cleanly.
///
/// A refused operation is not a reason to stop. It is sent back as an error
/// frame, and the loop continues. A malformed frame is different, because the
/// two sides no longer agree on the format, so the loop stops.
///
/// # Errors
///
/// Returns [`RpcError::Frame`] when the stream fails or the peer sends
/// something the frame layer refuses.
pub fn serve(stream: &mut (impl Read + Write), ops: &dyn FileOps) -> Result<(), RpcError> {
    loop {
        let frame = match read_frame(stream) {
            Ok(f) => f,
            Err(FrameError::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        if frame.kind != FrameKind::Request {
            return Err(RpcError::UnexpectedFrameKind(frame.kind));
        }

        // A payload that does not decode is answered, not ignored, so the
        // caller learns why instead of waiting forever.
        let reply = match Request::decode(&frame.payload) {
            Ok(request) => handle(ops, &request),
            Err(WireError::InvalidPath) => Err(OpError::InvalidPath),
            Err(WireError::TooLong) => Err(OpError::RangeTooLarge),
            Err(_) => Err(OpError::Unsupported),
        };

        let out = match reply {
            Ok(response) => Frame {
                kind: FrameKind::Response,
                request_id: frame.request_id,
                payload: response.encode(),
            },
            Err(error) => Frame {
                kind: FrameKind::Error,
                request_id: frame.request_id,
                payload: error.encode(),
            },
        };
        write_frame(stream, &out)?;
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::thread;

    use super::{Client, RpcError, exchange_hello, serve};
    use crate::frame::{Frame, FrameError, FrameKind, read_frame, write_frame};
    use crate::memfs::MemoryFs;
    use crate::ops::{FileKind, OpError, Request, Response};
    use crate::path::RemotePath;
    use crate::peers::DeviceKind;
    use crate::transport::{Endpoint, loopback};
    use crate::wire::{Encoder, WireError};

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    /// Serve `fs` on one loopback endpoint, in its own thread, and return a
    /// client connected to the other endpoint.
    ///
    /// A test must end by calling [`finish`], which drops the client so the
    /// server sees end of file, then joins the thread. Otherwise the thread
    /// would sit forever, waiting for a frame that never arrives.
    fn spawn_server(fs: MemoryFs) -> (Client<Endpoint>, thread::JoinHandle<Result<(), RpcError>>) {
        let (client_end, mut server_end) = loopback();
        let handle = thread::spawn(move || serve(&mut server_end, &fs));
        (Client::new(client_end), handle)
    }

    /// Drop the client, then join the server thread and check it ended
    /// cleanly.
    fn finish(client: Client<Endpoint>, handle: thread::JoinHandle<Result<(), RpcError>>) {
        drop(client);
        assert!(
            handle.join().unwrap().is_ok(),
            "serve should return Ok(()) once the client disconnects"
        );
    }

    #[test]
    fn list_reaches_the_server_and_returns_entries() {
        let fs = MemoryFs::new();
        fs.insert_file("DCIM/a.jpg", b"a".to_vec());
        let (mut client, handle) = spawn_server(fs);

        let (entries, next_cursor) = client.list(&path("DCIM"), 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.jpg");
        assert_eq!(next_cursor, None);

        finish(client, handle);
    }

    #[test]
    fn stat_reaches_the_server_and_describes_the_entry() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"hello".to_vec());
        let (mut client, handle) = spawn_server(fs);

        let entry = client.stat(&path("a.jpg")).unwrap();
        assert_eq!(entry.name, "a.jpg");
        assert_eq!(entry.size, 5);

        finish(client, handle);
    }

    #[test]
    fn read_reaches_the_server_and_returns_bytes() {
        let fs = MemoryFs::new();
        fs.insert_file("a.jpg", b"hello".to_vec());
        let (mut client, handle) = spawn_server(fs);

        let bytes = client.read(&path("a.jpg"), 1, 3).unwrap();
        assert_eq!(bytes, b"ell");

        finish(client, handle);
    }

    #[test]
    fn write_reaches_the_server_and_returns_the_byte_count() {
        let fs = MemoryFs::new();
        let (mut client, handle) = spawn_server(fs);

        let written = client.write(&path("a.txt"), 0, b"hi".to_vec()).unwrap();
        assert_eq!(written, 2);

        finish(client, handle);
    }

    #[test]
    fn truncate_reaches_the_server() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"hello".to_vec());
        let (mut client, handle) = spawn_server(fs);

        client.truncate(&path("a.txt"), 2).unwrap();
        let bytes = client.read(&path("a.txt"), 0, 10).unwrap();
        assert_eq!(bytes, b"he");

        finish(client, handle);
    }

    #[test]
    fn rename_then_stat_shows_the_file_at_its_new_name() {
        let fs = MemoryFs::new();
        fs.insert_file("old.txt", b"data".to_vec());
        let (mut client, handle) = spawn_server(fs);

        client.rename(&path("old.txt"), &path("new.txt")).unwrap();
        let entry = client.stat(&path("new.txt")).unwrap();
        assert_eq!(entry.name, "new.txt");
        assert!(matches!(
            client.stat(&path("old.txt")),
            Err(RpcError::Remote(OpError::NotFound))
        ));

        finish(client, handle);
    }

    #[test]
    fn set_mtime_reaches_the_server() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"data".to_vec());
        let (mut client, handle) = spawn_server(fs);

        client.set_mtime(&path("a.txt"), 12345).unwrap();
        let entry = client.stat(&path("a.txt")).unwrap();
        assert_eq!(entry.modified_unix_secs, 12345);

        finish(client, handle);
    }

    #[test]
    fn mkdir_reaches_the_server() {
        let fs = MemoryFs::new();
        let (mut client, handle) = spawn_server(fs);

        client.mkdir(&path("NewFolder")).unwrap();
        let entry = client.stat(&path("NewFolder")).unwrap();
        assert_eq!(entry.kind, FileKind::Directory);

        finish(client, handle);
    }

    #[test]
    fn delete_reaches_the_server() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"data".to_vec());
        let (mut client, handle) = spawn_server(fs);

        client.delete(&path("a.txt")).unwrap();
        assert!(matches!(
            client.stat(&path("a.txt")),
            Err(RpcError::Remote(OpError::NotFound))
        ));

        finish(client, handle);
    }

    #[test]
    fn a_refused_operation_comes_back_as_remote_not_found_and_the_connection_still_works() {
        let fs = MemoryFs::new();
        let (mut client, handle) = spawn_server(fs);

        let result = client.stat(&path("missing.txt"));
        assert!(matches!(result, Err(RpcError::Remote(OpError::NotFound))));

        // An error frame is not a reason to stop. The next call must still
        // reach the server.
        client.mkdir(&path("still-works")).unwrap();
        let entry = client.stat(&path("still-works")).unwrap();
        assert_eq!(entry.kind, FileKind::Directory);

        finish(client, handle);
    }

    #[test]
    fn reading_a_range_that_runs_past_the_end_returns_a_short_result() {
        let fs = MemoryFs::new();
        fs.insert_file("a.txt", b"hello".to_vec());
        let (mut client, handle) = spawn_server(fs);

        let bytes = client.read(&path("a.txt"), 3, 100).unwrap();
        assert_eq!(bytes, b"lo");

        finish(client, handle);
    }

    #[test]
    fn a_write_followed_by_a_read_returns_the_same_bytes() {
        let fs = MemoryFs::new();
        let (mut client, handle) = spawn_server(fs);

        client
            .write(&path("a.txt"), 0, b"hello world".to_vec())
            .unwrap();
        let bytes = client.read(&path("a.txt"), 0, 11).unwrap();
        assert_eq!(bytes, b"hello world");

        finish(client, handle);
    }

    #[test]
    fn delete_of_a_non_empty_directory_is_refused() {
        let fs = MemoryFs::new();
        fs.insert_file("DCIM/a.jpg", b"a".to_vec());
        let (mut client, handle) = spawn_server(fs);

        let result = client.delete(&path("DCIM"));
        assert!(matches!(result, Err(RpcError::Remote(OpError::NotEmpty))));

        finish(client, handle);
    }

    #[test]
    fn serve_returns_ok_when_the_client_disconnects_cleanly() {
        let fs = MemoryFs::new();
        let (client, handle) = spawn_server(fs);

        drop(client);
        assert!(handle.join().unwrap().is_ok());
    }

    #[test]
    fn a_peer_that_answers_the_wrong_question_is_refused() {
        // `serve` always answers the question it was asked, so a misbehaving
        // peer has to be built by hand here, one frame at a time, instead of
        // going through `serve`.
        let (client_end, mut fake_server) = loopback();
        let mut client = Client::new(client_end);

        let fake_server_thread = thread::spawn(move || {
            let request = read_frame(&mut fake_server).unwrap();
            assert_eq!(request.kind, FrameKind::Request);
            // The request identifier matches, so only the content of the
            // reply is wrong, not its bookkeeping. A `Read` response can
            // never be a sensible answer to a `Mkdir` request.
            let reply = Frame {
                kind: FrameKind::Response,
                request_id: request.request_id,
                payload: Response::Read { bytes: Vec::new() }.encode(),
            };
            write_frame(&mut fake_server, &reply).unwrap();
        });

        // A mkdir reply carries no bytes at all. The read reply carries a
        // length prefix, so decoding it as a mkdir reply finds bytes left
        // over. The mismatch cannot get past the decoder.
        let result = client.call(&Request::Mkdir { path: path("DCIM") });
        assert!(matches!(
            result,
            Err(RpcError::Wire(WireError::TrailingBytes))
        ));

        fake_server_thread.join().unwrap();
    }

    #[test]
    fn hello_carries_each_name_and_kind_to_the_other_side() {
        let (mut pixel_side, mut vamana_side) = loopback();
        let pixel_thread =
            thread::spawn(move || exchange_hello(&mut pixel_side, "Pixel 3 XL", DeviceKind::Phone));

        let (vamana_name, vamana_kind) =
            exchange_hello(&mut vamana_side, "Vamana", DeviceKind::Mac).unwrap();
        let (pixel_name, pixel_kind) = pixel_thread.join().unwrap().unwrap();

        assert_eq!(pixel_name, "Vamana");
        assert_eq!(pixel_kind, DeviceKind::Mac);
        assert_eq!(vamana_name, "Pixel 3 XL");
        assert_eq!(vamana_kind, DeviceKind::Phone);
    }

    #[test]
    fn a_name_over_64_bytes_is_refused_on_send() {
        let (mut sender, mut receiver) = loopback();
        let too_long = "a".repeat(65);

        assert!(matches!(
            exchange_hello(&mut sender, &too_long, DeviceKind::Phone),
            Err(RpcError::BadName)
        ));

        // Nothing was written, so once the sender is gone the receiver sees
        // end of file rather than a frame.
        drop(sender);
        match read_frame(&mut receiver) {
            Err(FrameError::Io(e)) => assert_eq!(e.kind(), ErrorKind::UnexpectedEof),
            other => panic!("expected end of file, got {other:?}"),
        }
    }

    #[test]
    fn a_name_over_64_bytes_is_refused_on_receive() {
        let (mut sender, mut receiver) = loopback();
        let mut encoder = Encoder::new();
        encoder.text(&"a".repeat(65));
        let oversized_payload = encoder.finish();
        write_frame(
            &mut sender,
            &Frame {
                kind: FrameKind::Hello,
                request_id: 0,
                payload: oversized_payload,
            },
        )
        .unwrap();

        // The oversized name never reaches the sender's own name check: the
        // decoder's length cap catches it first, so this is the error that
        // comes back, not `RpcError::BadName`.
        assert!(matches!(
            exchange_hello(&mut receiver, "Pixel 3 XL", DeviceKind::Phone),
            Err(RpcError::Wire(WireError::TooLong))
        ));
    }

    #[test]
    fn an_unknown_kind_byte_is_refused() {
        let (mut sender, mut receiver) = loopback();
        let mut encoder = Encoder::new();
        encoder.text("Pixel 3 XL");
        // No `DeviceKind` variant names this byte.
        encoder.u8(99);
        write_frame(
            &mut sender,
            &Frame {
                kind: FrameKind::Hello,
                request_id: 0,
                payload: encoder.finish(),
            },
        )
        .unwrap();

        assert!(matches!(
            exchange_hello(&mut receiver, "Vamana", DeviceKind::Mac),
            Err(RpcError::Wire(WireError::UnknownTag(99)))
        ));
    }

    #[test]
    fn a_name_with_a_control_character_is_refused() {
        let (mut sender, _receiver) = loopback();
        assert!(matches!(
            exchange_hello(&mut sender, "Pixel\u{0007}", DeviceKind::Phone),
            Err(RpcError::BadName)
        ));
    }

    #[test]
    fn a_request_where_a_hello_belongs_is_refused() {
        let (mut sender, mut receiver) = loopback();
        write_frame(
            &mut sender,
            &Frame {
                kind: FrameKind::Request,
                request_id: 1,
                payload: Request::Stat {
                    path: path("a.txt"),
                }
                .encode(),
            },
        )
        .unwrap();

        assert!(matches!(
            exchange_hello(&mut receiver, "Pixel 3 XL", DeviceKind::Phone),
            Err(RpcError::UnexpectedFrameKind(FrameKind::Request))
        ));
    }

    #[test]
    fn an_empty_name_is_refused() {
        let (mut sender, _receiver) = loopback();
        assert!(matches!(
            exchange_hello(&mut sender, "", DeviceKind::Phone),
            Err(RpcError::BadName)
        ));
    }
}
