//! Calling file operations across a connection, and serving them.
//!
//! [`FileOps`] is what a device offers. [`Client`] calls it across a stream.
//! [`serve`] answers those calls.
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

use crate::frame::{Frame, FrameError, FrameKind, read_frame, write_frame};
use crate::ops::{Entry, OpError, Request, Response};
use crate::path::RemotePath;
use crate::wire::WireError;

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
    /// is not recursive in version 1.
    fn delete(&self, path: &RemotePath) -> Result<(), OpError>;
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
    #[error("unexpected frame kind {0:?}")]
    UnexpectedFrameKind(FrameKind),
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
            FrameKind::Request => Err(RpcError::UnexpectedFrameKind(FrameKind::Request)),
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
    use std::thread;

    use super::{Client, RpcError, serve};
    use crate::frame::{Frame, FrameKind, read_frame, write_frame};
    use crate::memfs::MemoryFs;
    use crate::ops::{FileKind, OpError, Request, Response};
    use crate::path::RemotePath;
    use crate::transport::{Endpoint, loopback};
    use crate::wire::WireError;

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
}
