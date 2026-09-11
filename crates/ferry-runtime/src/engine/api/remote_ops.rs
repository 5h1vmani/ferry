//! The file operations a paired device answers: `list` and `stat`, and
//! the four that read or write bytes on the peer.

use ferry_core::limits::{MAX_READ_LEN, MAX_WRITE_LEN};
use ferry_core::path::RemotePath;

use crate::access;
use crate::engine::{Engine, entry_from_core, record_this, remote_call};
use crate::errors::{failed, from_path};
use crate::folder;
use crate::{Entry, FerryError};

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
    /// List every entry in one folder on a paired device.
    ///
    /// Borrows one of the device's four pooled connections, then pages
    /// through the server's cursor until it reports no more entries, and
    /// returns them in the order the server sent them. This blocks for one
    /// round trip per page, so the app must call it off the main thread.
    ///
    /// `docs/engine-contract.md`, item 19: the pool is the engine's, shared
    /// with the `WebDAV` bridge, so two listings in a row reuse one
    /// connection rather than dialling twice.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when the path is refused,
    /// `Runtime::NotPaired` when that device is not stored,
    /// `Runtime::NotStarted` before [`Engine::start`] has run,
    /// `Runtime::NotReachable` when no dial succeeds, an `OpError` code
    /// when the peer refuses, such as `OpError::NotFound` for a folder that
    /// does not exist, and `Runtime::FolderTooLarge` when the peer pages the
    /// folder past the bounds `folder::after_page` checks, such as a
    /// `next_cursor` that never advances.
    pub fn list(
        &self,
        device_key_hex: String,
        remote_path: String,
    ) -> Result<Vec<Entry>, FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;

        let mut entries = Vec::new();
        let mut cursor = 0u64;
        let mut pages = 0usize;
        let mut entries_seen = 0usize;
        loop {
            let (page, next_cursor) = borrowed.call(|client| client.list(&path, cursor))?;
            pages += 1;
            entries_seen += page.len();
            entries.extend(page.into_iter().map(entry_from_core));
            match folder::after_page(cursor, next_cursor, pages, entries_seen) {
                Ok(Some(next)) => cursor = next,
                Ok(None) => break,
                Err(folder::ListTooLarge) => return Err(failed("Runtime::FolderTooLarge")),
            }
        }
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::List,
            path.as_str(),
            None,
            Some(u32::try_from(entries.len()).unwrap_or(u32::MAX)),
            None,
        );
        Ok(entries)
    }

    /// Describe one file or folder on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. The phone's `DocumentsProvider`
    /// answers `queryDocument` with this. Blocks for one round trip, so the
    /// app calls it off the main thread, as it does [`Engine::list`].
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], including `OpError::NotFound` for a path that
    /// names no file.
    pub fn stat(&self, device_key_hex: String, remote_path: String) -> Result<Entry, FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        let entry = borrowed.call(|client| client.stat(&path))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Stat,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(entry_from_core(entry))
    }

    /// Read a byte range from a file on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. At most [`MAX_READ_LEN`] bytes,
    /// one mebibyte. A longer ask is clamped, not refused, so the caller
    /// gets a short read, which is an ordinary read result: fewer bytes
    /// than asked for also means the end of the file.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], including `OpError::IsADirectory` when the path
    /// names a folder.
    pub fn read_at(
        &self,
        device_key_hex: String,
        remote_path: String,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let want = len.min(MAX_READ_LEN);
        let mut borrowed = pool.take_dialing(&self.shared)?;
        let bytes = borrowed.call(|client| client.read(&path, offset, want))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Read,
            path.as_str(),
            Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            None,
            None,
        );
        Ok(bytes)
    }

    /// Write a byte range to a file on a paired device, creating the file
    /// when it does not exist.
    ///
    /// `docs/engine-contract.md`, item 19. More than [`MAX_WRITE_LEN`]
    /// bytes, one mebibyte, in one call is refused before anything reaches
    /// the wire, so a refused call writes nothing.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `Runtime::WriteTooLarge` when `bytes` is
    /// longer than one mebibyte, and `OpError::PermissionDenied` when the
    /// peer's root is not writable.
    pub fn write_at(
        &self,
        device_key_hex: String,
        remote_path: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        // Before the pool is touched, so a refused write neither dials nor
        // sends a byte.
        if bytes.len() > MAX_WRITE_LEN as usize {
            return Err(failed("Runtime::WriteTooLarge"));
        }
        let mut borrowed = pool.take_dialing(&self.shared)?;
        let written = borrowed.call(|client| client.write(&path, offset, bytes))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Write,
            path.as_str(),
            Some(u64::from(written)),
            None,
            None,
        );
        Ok(())
    }

    /// Set a file's length on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. The phone's provider truncates
    /// to zero when it opens a document in a truncating mode.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::PermissionDenied` when the
    /// peer's root is not writable.
    pub fn truncate(
        &self,
        device_key_hex: String,
        remote_path: String,
        len: u64,
    ) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.truncate(&path, len))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Truncate,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Make one folder on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. Makes one level only: the parent
    /// must already exist, or the peer answers `OpError::NotFound`.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::AlreadyExists` when something is
    /// already there, and `OpError::PermissionDenied` when the peer's root
    /// is not writable.
    pub fn mkdir(&self, device_key_hex: String, remote_path: String) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.mkdir(&path))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Mkdir,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Delete one file, or one empty folder, on a paired device.
    ///
    /// `docs/engine-contract.md`, item 19. The wire has no recursive
    /// delete, so a folder with anything in it is refused with
    /// `OpError::NotEmpty`. A caller that wants the folder gone walks it
    /// and deletes the leaves first, as the `WebDAV` bridge does.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::NotEmpty` for a folder that
    /// still holds something, and `OpError::PermissionDenied` when the
    /// peer's root is not writable.
    pub fn delete(&self, device_key_hex: String, remote_path: String) -> Result<(), FerryError> {
        let (path, pool) = remote_call(&self.shared, &device_key_hex, &remote_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.delete(&path))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Delete,
            path.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }

    /// Move or rename a file or folder on a paired device, within one root.
    ///
    /// `docs/engine-contract.md`, item 19. Across two roots the peer
    /// answers `OpError::Unsupported`, the same refusal the `WebDAV`
    /// bridge turns into 502.
    ///
    /// # Errors
    ///
    /// As [`Engine::list`], plus `OpError::Unsupported` for a move across
    /// roots, `OpError::AlreadyExists` when something is already at `to`,
    /// and `OpError::PermissionDenied` when the peer's root is not
    /// writable.
    pub fn rename(
        &self,
        device_key_hex: String,
        from: String,
        to: String,
    ) -> Result<(), FerryError> {
        let (source, pool) = remote_call(&self.shared, &device_key_hex, &from)?;
        let destination = RemotePath::parse(&to).map_err(from_path)?;
        let mut borrowed = pool.take_dialing(&self.shared)?;
        borrowed.call(|client| client.rename(&source, &destination))?;
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Rename,
            // The destination, not the source, the same choice
            // `guard.rs` makes on the serving side: a person searching the
            // log looks for where a file ended up.
            destination.as_str(),
            None,
            None,
            None,
        );
        Ok(())
    }
}
