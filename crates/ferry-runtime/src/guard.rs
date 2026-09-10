//! Small wrappers that let the engine take something back, or break something
//! on purpose.
//!
//! [`GuardedFs`] can be switched off, so `forget` really does stop a device
//! from reading files. [`StopAware`] fails the next read or write once the
//! engine is stopping, so a transfer thread ends instead of finishing a whole
//! file first. [`Cut`] fails a stream after a chosen number of bytes, so a
//! test can break a transfer's link at an exact point.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ferry_core::ops::{Entry, OpError};
use ferry_core::path::RemotePath;
use ferry_core::roots::Roots;
use ferry_core::rpc::FileOps;

use crate::state::lock;

/// The served roots, and the spec list [`crate::Engine::roots`] last
/// reported.
///
/// Bundled together so [`crate::Engine::set_roots`] swaps both at once,
/// under one lock: nothing ever reads a spec list that does not match the
/// [`Roots`] behind it.
pub(crate) struct RootsState {
    /// What [`crate::Engine::roots`] returns.
    pub(crate) specs: Vec<crate::Root>,
    /// The opened roots, actually serving files.
    pub(crate) opened: Arc<Roots>,
}

/// A handle every connection currently serving a peer shares.
///
/// docs/engine-contract.md, batch C, item 15, requires a root change to
/// reach an already-connected peer on its very next operation, with no
/// reconnect. Holding this handle rather than a snapshot of [`Roots`] is
/// what makes that true: [`GuardedFs`] takes the lock fresh on every call,
/// clones out whichever [`Arc<Roots>`] is behind it at that moment, and
/// drops the lock before doing any I/O. `set_roots` takes the same lock only
/// long enough to swap the value in. So a swap is one quick pointer
/// replacement, never blocked behind a read or a write in flight, and the
/// next operation on every open connection sees it at once.
pub(crate) type RootsHandle = Arc<Mutex<Option<RootsState>>>;

/// The served roots, given to one connection, with an off switch.
///
/// A connection that is already serving cannot be closed from outside. The
/// socket lives inside the encrypted stream, and this crate has no handle on
/// it. So `forget` flips the switch instead. The socket stays open until the
/// peer goes away or the idle timeout fires, but every operation on it is
/// refused from the moment the switch goes off.
pub(crate) struct GuardedFs {
    roots: RootsHandle,
    allowed: Arc<AtomicBool>,
}

impl GuardedFs {
    /// Wrap the served roots for one connection.
    pub(crate) fn new(roots: RootsHandle, allowed: Arc<AtomicBool>) -> Self {
        Self { roots, allowed }
    }

    /// The roots to use for this call, read fresh: an error once this
    /// connection may not act, or once nothing is being served at all.
    fn current(&self) -> Result<Arc<Roots>, OpError> {
        if !self.allowed.load(Ordering::SeqCst) {
            return Err(OpError::PermissionDenied);
        }
        lock(&self.roots)
            .as_ref()
            .map(|state| Arc::clone(&state.opened))
            .ok_or(OpError::PermissionDenied)
    }
}

impl FileOps for GuardedFs {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        self.current()?.list(path, cursor)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        self.current()?.stat(path)
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        self.current()?.read(path, offset, length)
    }

    fn write(&self, path: &RemotePath, offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        self.current()?.write(path, offset, bytes)
    }

    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError> {
        self.current()?.truncate(path, length)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError> {
        self.current()?.rename(from, to)
    }

    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError> {
        self.current()?.set_mtime(path, modified_unix_secs)
    }

    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError> {
        self.current()?.mkdir(path)
    }

    fn delete(&self, path: &RemotePath) -> Result<(), OpError> {
        self.current()?.delete(path)
    }
}

/// A stream that fails once the engine is stopping.
///
/// A transfer reads one chunk at a time. Between chunks this wrapper is
/// asked for more bytes, sees the flag, and returns an error. The transfer
/// then ends with a connection error, which its retry loop already knows how
/// to handle. Without this, `stop` would wait for a whole file.
pub(crate) struct StopAware<S> {
    inner: S,
    stopping: Arc<AtomicBool>,
}

impl<S> StopAware<S> {
    /// Wrap a stream so it fails once `stopping` is set.
    pub(crate) fn new(inner: S, stopping: Arc<AtomicBool>) -> Self {
        Self { inner, stopping }
    }

    /// An error that says the engine asked this stream to stop.
    ///
    /// The kind is part of the contract. `Read::read_exact` and
    /// `Write::write_all` treat `Interrupted` as "try again", so a stream
    /// that reported that kind was asked again at once, for ever, and `stop`
    /// waited on a thread spinning at full speed. `ConnectionAborted` is
    /// never retried, so the call fails and the thread ends.
    fn stopping_error() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionAborted, "the engine is stopping")
    }
}

impl<S: Read> Read for StopAware<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(Self::stopping_error());
        }
        self.inner.read(out)
    }
}

impl<S: Write> Write for StopAware<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(Self::stopping_error());
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A stream that fails once a chosen number of bytes have crossed it.
///
/// It counts every byte read and every byte written, in the order they
/// happen, read and write sharing one count. Once the count reaches the
/// configured limit, the stream fails with [`io::ErrorKind::ConnectionAborted`].
/// A read or a write that would cross the limit is cut short at the limit
/// instead, so the failure lands on exactly that byte, whatever size the
/// caller's buffer is or the underlying stream hands back.
///
/// This is how `tests/resume_sweep.rs` breaks a transfer's link at an exact
/// byte, so a sweep over many cut points can prove that a resume refetches at
/// most one chunk, however and whenever the link broke. Every byte is also
/// added to a running total kept in `Shared`, whether or not a limit is
/// armed, so a test can read back how much a pull cost on the wire.
pub(crate) struct Cut<S> {
    inner: S,
    /// The byte count this stream fails at, once reached. `None` means this
    /// dial was not chosen for a cut.
    limit: Option<u64>,
    /// Bytes this instance has passed, read and write combined, in order.
    counted: u64,
    /// Every byte is added here too, cut or not.
    wire_bytes: Arc<AtomicU64>,
}

impl<S> Cut<S> {
    /// Wrap a stream. `limit` is `None` for an ordinary dial. `wire_bytes` is
    /// the running total that [`crate::Engine::wire_bytes`] reads back.
    pub(crate) fn new(inner: S, limit: Option<u64>, wire_bytes: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            limit,
            counted: 0,
            wire_bytes,
        }
    }

    /// An error that says this stream was cut at the byte the test chose.
    ///
    /// The kind matches [`StopAware`]'s: never retried by `read_exact` or
    /// `write_all`, so the call fails and the attempt ends instead of
    /// spinning.
    fn cut_error() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionAborted, "the wire was cut")
    }

    /// How many more bytes may cross before the limit, or `None` when no
    /// limit is armed.
    fn left(&self) -> Option<u64> {
        self.limit.map(|limit| limit.saturating_sub(self.counted))
    }

    /// Note that `n` more bytes crossed, cut or not.
    fn account(&mut self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        self.counted = self.counted.saturating_add(n);
        self.wire_bytes.fetch_add(n, Ordering::SeqCst);
    }
}

impl<S: Read> Read for Cut<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some(left) = self.left() else {
            let n = self.inner.read(out)?;
            self.account(n);
            return Ok(n);
        };
        if left == 0 {
            return Err(Self::cut_error());
        }
        // Ask the inner stream for no more than what remains before the cut,
        // so a read that would cross it lands short instead.
        let cap = usize::try_from(left).unwrap_or(out.len()).min(out.len());
        let n = self.inner.read(&mut out[..cap])?;
        self.account(n);
        Ok(n)
    }
}

impl<S: Write> Write for Cut<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(left) = self.left() else {
            let n = self.inner.write(bytes)?;
            self.account(n);
            return Ok(n);
        };
        if left == 0 {
            return Err(Self::cut_error());
        }
        let cap = usize::try_from(left)
            .unwrap_or(bytes.len())
            .min(bytes.len());
        let n = self.inner.write(&bytes[..cap])?;
        self.account(n);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
