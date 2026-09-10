//! Two small wrappers that let the engine take something back.
//!
//! [`GuardedFs`] can be switched off, so `forget` really does stop a device
//! from reading files. [`StopAware`] fails the next read or write once the
//! engine is stopping, so a transfer thread ends instead of finishing a whole
//! file first.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferry_core::localfs::LocalFs;
use ferry_core::ops::{Entry, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::FileOps;

/// The shared root, served to one connection, with an off switch.
///
/// A connection that is already serving cannot be closed from outside. The
/// socket lives inside the encrypted stream, and this crate has no handle on
/// it. So `forget` flips the switch instead. The socket stays open until the
/// peer goes away or the idle timeout fires, but every operation on it is
/// refused from the moment the switch goes off.
pub(crate) struct GuardedFs {
    inner: Arc<LocalFs>,
    allowed: Arc<AtomicBool>,
}

impl GuardedFs {
    /// Wrap the shared root for one connection.
    pub(crate) fn new(inner: Arc<LocalFs>, allowed: Arc<AtomicBool>) -> Self {
        Self { inner, allowed }
    }

    /// `Ok` while this connection may still act, an error once it may not.
    fn check(&self) -> Result<(), OpError> {
        if self.allowed.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(OpError::PermissionDenied)
        }
    }
}

impl FileOps for GuardedFs {
    fn list(&self, path: &RemotePath, cursor: u64) -> Result<(Vec<Entry>, Option<u64>), OpError> {
        self.check()?;
        self.inner.list(path, cursor)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry, OpError> {
        self.check()?;
        self.inner.stat(path)
    }

    fn read(&self, path: &RemotePath, offset: u64, length: u32) -> Result<Vec<u8>, OpError> {
        self.check()?;
        self.inner.read(path, offset, length)
    }

    fn write(&self, path: &RemotePath, offset: u64, bytes: &[u8]) -> Result<u32, OpError> {
        self.check()?;
        self.inner.write(path, offset, bytes)
    }

    fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), OpError> {
        self.check()?;
        self.inner.truncate(path, length)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), OpError> {
        self.check()?;
        self.inner.rename(from, to)
    }

    fn set_mtime(&self, path: &RemotePath, modified_unix_secs: i64) -> Result<(), OpError> {
        self.check()?;
        self.inner.set_mtime(path, modified_unix_secs)
    }

    fn mkdir(&self, path: &RemotePath) -> Result<(), OpError> {
        self.check()?;
        self.inner.mkdir(path)
    }

    fn delete(&self, path: &RemotePath) -> Result<(), OpError> {
        self.check()?;
        self.inner.delete(path)
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
    fn stopping_error() -> io::Error {
        io::Error::new(io::ErrorKind::Interrupted, "the engine is stopping")
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
