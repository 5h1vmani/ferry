//! Landing a file onto the peer through the bridge: `PUT` of a real file,
//! and `COPY` of a file, which lands the same way once its bytes are on
//! this Mac.
//!
//! `docs/engine-contract.md`, item 6, I2, "I2, saving", reusing the
//! sender's landing rule from item 5 (`push.rs`): a spool file on this
//! Mac's own disk, its manifest built once, then chunks written to the
//! peer, verified by the peer's own manifest, and landed. The wire
//! plumbing this needs, `write_all_remote` and `partial_path`, is
//! `push.rs`'s own; this module calls it rather than carrying a copy.
//!
//! # New file versus delta
//!
//! A destination that does not exist yet is landed the push way: chunks to
//! `<name>.ferry-part`, verified by the peer's manifest of the partial,
//! renamed, `set_mtime`. A destination that already exists is landed in
//! place instead: the peer's manifest of the real file is compared
//! chunk by chunk against the spool's own manifest, and only the chunks
//! that differ are written, before a `truncate` to the new length and a
//! final manifest check. That is the delta on save.
//!
//! Neither path resumes across attempts the way a background transfer
//! does. A `PUT` or a `COPY` is one HTTP request. `docs/engine-contract.md`,
//! item 6: "a landing removes the spool file before its answer goes out,
//! whether the landing failed or succeeded." Finder is the one that
//! retries, the same way it retries any other failed request.
//!
//! The spool file itself is a [`SpoolFile`], whose `Drop` removes it: a
//! short body, a dropped connection, or any other early return between
//! [`new_spool_path`] and a landing's own cleanup must never leave a spool
//! file behind, per that same rule. Every caller that reaches a landing
//! also drops its `SpoolFile` explicitly, right before it writes any
//! answer, on success and on failure: the file is gone before the answer
//! reaches Finder, not merely by the time the handler function returns.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferry_core::chunk::Manifest;
use ferry_core::limits;
use ferry_core::localfs::LocalFs;
use ferry_core::ops::OpError;
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, RpcError};

use crate::engine::Shared;
use crate::push::{partial_path, write_all_remote};

use super::http;

/// The most the spool folder, `data_dir/dav_spool/`, may hold at once,
/// across every device. Past this, [`new_spool_path`] answers
/// [`SpoolError::Full`] rather than creating another file: a `PUT` this
/// bridge cannot yet land must not be allowed to fill the disk with spool
/// files while it waits.
pub(crate) const MAX_SPOOL_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// The spool folder's running total, across every device, checked against
/// [`MAX_SPOOL_BYTES`] instead of a fresh recursive walk of the folder on
/// every `PUT` and `COPY` (`docs/audits/fable-engineering.md`, finding 4;
/// walking on every request made the cost of a Finder copy grow with the
/// square of the file count).
///
/// One instance lives on `dav::MountRegistry`, for the life of the engine.
/// [`SpoolBytes::init_from`] walks the folder once, the first time any
/// bridge starts; after that, [`new_spool_path`] adds a created file's
/// reserved size and [`SpoolFile`]'s `Drop`, together with [`sweep_spool`],
/// subtract a removed one's, so the total stays exact without walking
/// again.
pub(crate) struct SpoolBytes {
    total: AtomicU64,
    walked: AtomicBool,
}

impl SpoolBytes {
    pub(crate) const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            walked: AtomicBool::new(false),
        }
    }

    /// Walks `root` once, the very first time this is called for this
    /// instance. Every call after that is a no-op: `add` and `sub` have
    /// kept the total exact since the first walk.
    pub(crate) fn init_from(&self, root: &Path) {
        if self.walked.swap(true, Ordering::SeqCst) {
            return;
        }
        self.total.store(dir_bytes(root), Ordering::SeqCst);
    }

    /// The running total right now.
    pub(crate) fn get(&self) -> u64 {
        self.total.load(Ordering::SeqCst)
    }

    fn add(&self, bytes: u64) {
        self.total.fetch_add(bytes, Ordering::SeqCst);
    }

    /// Subtracts `bytes`, saturating at zero: a mismatch between what was
    /// added and what is actually removed must never wrap this past
    /// `u64::MAX` and make the cap look free when the folder is, in truth,
    /// still full.
    fn sub(&self, bytes: u64) {
        let _ = self
            .total
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_sub(bytes))
            });
    }
}

/// A spool file's path and its reserved size, removed the moment this
/// drops. Every caller of [`new_spool_path`] holds one of these for exactly
/// as long as the spool file may exist: a short body, a dropped connection,
/// or any other early return cleans it up the same way a successful
/// landing's own explicit `drop` does, and lowers [`SpoolBytes`] by the same
/// amount [`new_spool_path`] raised it by. `docs/engine-contract.md`, item
/// 6: "a landing removes the spool file before its answer goes out,
/// whether the landing failed or succeeded."
pub(crate) struct SpoolFile {
    path: PathBuf,
    size: u64,
    shared: Arc<Shared>,
}

impl SpoolFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SpoolFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.shared.mounts.spool_bytes.sub(self.size);
    }
}

/// Why [`new_spool_path`] could not make a fresh spool file.
pub(crate) enum SpoolError {
    /// The spool folder already holds [`MAX_SPOOL_BYTES`] or more. The
    /// caller answers 507, the same code the lock table's own cap uses.
    Full,
    /// The folder could not be made, or a name could not be generated.
    /// Nothing on the wire caused this, so the caller answers 500.
    Failed,
}

/// Creates a fresh, empty spool file under
/// `data_dir/dav_spool/<device key hex>/` with a random name, and returns
/// it. `expected_len` is the number of bytes this landing will write into
/// it (a `PUT`'s `Content-Length`, or a `COPY`'s source size): reserved in
/// [`SpoolBytes`] the moment this returns `Ok`, and released by
/// [`SpoolFile`]'s `Drop`, whether or not the landing that follows
/// succeeds.
///
/// # Errors
///
/// Returns [`SpoolError::Full`] when the spool folder's running total is
/// already at or past [`MAX_SPOOL_BYTES`], and [`SpoolError::Failed`] when
/// the folder cannot be made or a name cannot be generated.
pub(crate) fn new_spool_path(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    expected_len: u64,
) -> Result<SpoolFile, SpoolError> {
    let root = shared.data_dir.join("dav_spool");
    let dir = root.join(device_key_hex);
    std::fs::create_dir_all(&dir).map_err(|_| SpoolError::Failed)?;
    if shared.mounts.spool_bytes.get() >= MAX_SPOOL_BYTES {
        return Err(SpoolError::Full);
    }
    let name = super::random_hex(16).map_err(|_| SpoolError::Failed)?;
    shared.mounts.spool_bytes.add(expected_len);
    Ok(SpoolFile {
        path: dir.join(name),
        size: expected_len,
        shared: Arc::clone(shared),
    })
}

/// Removes every leftover file under `data_dir/dav_spool/<device key
/// hex>/`, left behind by a crash or an ungraceful quit before this
/// device's bridge last stopped, and lowers [`SpoolBytes`] by what was
/// removed. Called once, at `mount_start`, only on a bridge that is not
/// already running: a live bridge's own spool files are never swept out
/// from under it.
pub(crate) fn sweep_spool(shared: &Shared, device_key_hex: &str) {
    let dir = shared.data_dir.join("dav_spool").join(device_key_hex);
    let removed = dir_bytes(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    shared.mounts.spool_bytes.sub(removed);
}

/// The combined size of every file under `dir`, walked recursively.
///
/// Best effort: a folder that cannot be read contributes 0 rather than
/// failing outright, so a transient I/O error here never wrongly refuses an
/// otherwise healthy `PUT`. Used only by [`SpoolBytes::init_from`], once,
/// and by [`sweep_spool`] to learn what it is about to remove; no longer
/// called on every `PUT` or `COPY` (`docs/audits/fable-engineering.md`,
/// finding 4).
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            total += dir_bytes(&entry.path());
        } else {
            total += metadata.len();
        }
    }
    total
}

/// Receives exactly `content_length` bytes from `reader` into a fresh
/// file at `spool_path`, in bounded pieces, never holding the whole body
/// in memory. `docs/engine-contract.md`, item 6, I2.
///
/// # Errors
///
/// Returns an error when the file cannot be created or written, or when
/// fewer than `content_length` bytes arrive: the connection ended before
/// the declared body did, so its framing can no longer be trusted for a
/// next request, the same reasoning `server::stream_body`'s own S2
/// carries for a `GET`.
pub(crate) fn spool_body(
    reader: &mut impl BufRead,
    content_length: u64,
    spool_path: &Path,
) -> io::Result<()> {
    let mut file = std::fs::File::create(spool_path)?;
    let copied = http::copy_body(reader, content_length, &mut file)?;
    if copied != content_length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the request body ended before its declared Content-Length",
        ));
    }
    Ok(())
}

/// Opens the spool file at `spool_path` as its own [`LocalFs`] root and
/// builds its manifest. `None` on any failure, in which case the caller
/// answers 500 and removes the spool file.
pub(crate) fn open_spool(spool_path: &Path) -> Option<(LocalFs, RemotePath, Manifest)> {
    let dir = spool_path.parent()?;
    let name = spool_path.file_name()?.to_str()?;
    let fs = LocalFs::open(dir).ok()?;
    let leaf = RemotePath::parse(name).ok()?;
    let manifest = fs.manifest(&leaf).ok()?;
    Some((fs, leaf, manifest))
}

/// Fetches `source`'s whole content from the peer into a fresh spool
/// file at `spool_path`, in pieces of at most
/// [`ferry_core::limits::MAX_READ_LEN`]. Used by `COPY`: "read from the
/// peer", before the same landing rule pushes it back under the new
/// name.
///
/// # Errors
///
/// As [`Client::read`], and when the spool file cannot be written to.
pub(crate) fn fetch_into_spool<S: Read + Write>(
    client: &mut Client<S>,
    source: &RemotePath,
    spool_path: &Path,
    size: u64,
) -> Result<(), RpcError> {
    let mut file =
        std::fs::File::create(spool_path).map_err(|_| RpcError::Remote(OpError::Internal))?;
    let piece = limits::MAX_READ_LEN;
    let mut offset = 0u64;
    while offset < size {
        let want = u32::try_from((size - offset).min(u64::from(piece))).unwrap_or(piece);
        let bytes = client.read(source, offset, want)?;
        if bytes.is_empty() {
            break;
        }
        file.write_all(&bytes)
            .map_err(|_| RpcError::Remote(OpError::Internal))?;
        offset += u64::try_from(bytes.len()).unwrap_or(0);
    }
    Ok(())
}

/// Lands a spool file at `destination`, which does not exist there yet:
/// the push rule from `docs/engine-contract.md`, item 5. Every chunk to
/// `<destination>.ferry-part`, verified by the peer's own manifest of the
/// partial, renamed onto `destination`, then `set_mtime`.
///
/// A failure tries one `delete` of the partial before returning: a `PUT`
/// is one HTTP request with no resume, so a partial this attempt cannot
/// finish is not worth keeping around for a next one to find. The delete
/// itself is best effort; its own failure is not reported, since the
/// original error is the one the caller needs.
///
/// `bytes_written` is increased by every byte actually sent to the peer,
/// whether or not this call ends up returning `Ok`: the caller logs it
/// either way, so a failed landing's access log entry says what was
/// really written, not nothing.
///
/// # Errors
///
/// Returns [`RpcError::Remote`] with [`OpError::Internal`] when the
/// landed file's manifest does not match what was sent, and forwards any
/// other RPC failure.
pub(crate) fn land_new<S: Read + Write>(
    client: &mut Client<S>,
    destination: &RemotePath,
    spool_fs: &LocalFs,
    spool_leaf: &RemotePath,
    manifest: &Manifest,
    bytes_written: &mut u64,
) -> Result<(), RpcError> {
    let partial = partial_path(destination).map_err(|_| RpcError::Remote(OpError::InvalidPath))?;
    match land_new_partial(
        client,
        destination,
        &partial,
        spool_fs,
        spool_leaf,
        manifest,
        bytes_written,
    ) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = client.delete(&partial);
            Err(error)
        }
    }
}

fn land_new_partial<S: Read + Write>(
    client: &mut Client<S>,
    destination: &RemotePath,
    partial: &RemotePath,
    spool_fs: &LocalFs,
    spool_leaf: &RemotePath,
    manifest: &Manifest,
    bytes_written: &mut u64,
) -> Result<(), RpcError> {
    if manifest.length() == 0 {
        // An empty file still needs its partial to exist, so the rename
        // below has something to rename.
        client.write(partial, 0, Vec::new())?;
    }
    for index in 0..manifest.chunk_count() {
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = spool_fs
            .read(spool_leaf, offset, length)
            .map_err(RpcError::Remote)?;
        write_all_remote(client, partial, offset, &bytes, bytes_written)?;
    }
    client.truncate(partial, manifest.length())?;

    let landed = client.manifest(partial)?;
    if landed != *manifest {
        return Err(RpcError::Remote(OpError::Internal));
    }
    client.rename(partial, destination)?;
    client.set_mtime(destination, crate::state::now_unix_secs())?;
    Ok(())
}

/// Lands a spool file at `destination`, which already exists there: the
/// delta on save from `docs/engine-contract.md`, item 6, I2. Only the
/// chunks whose chaining values differ from the peer's own manifest are
/// written, in place, before a `truncate` to the new length, `set_mtime`,
/// and a final manifest check.
///
/// `bytes_written` is increased by every byte actually sent to the peer,
/// whether or not this call ends up returning `Ok`, the same as
/// [`land_new`].
///
/// # Errors
///
/// Returns [`RpcError::Remote`] with [`OpError::Internal`] when the final
/// manifest still does not match, and forwards any other RPC failure.
pub(crate) fn land_delta<S: Read + Write>(
    client: &mut Client<S>,
    destination: &RemotePath,
    spool_fs: &LocalFs,
    spool_leaf: &RemotePath,
    manifest: &Manifest,
    bytes_written: &mut u64,
) -> Result<(), RpcError> {
    let remote = client.manifest(destination)?;
    let common = manifest.chunk_count().min(remote.chunk_count());
    for index in 0..manifest.chunk_count() {
        if index < common && manifest.chunks()[index] == remote.chunks()[index] {
            continue;
        }
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = spool_fs
            .read(spool_leaf, offset, length)
            .map_err(RpcError::Remote)?;
        write_all_remote(client, destination, offset, &bytes, bytes_written)?;
    }
    client.truncate(destination, manifest.length())?;

    // The manifest is checked before the modified time is touched: a file
    // that changed on the peer between its manifest and these writes fails
    // here, and must keep its old modified time, not one this landing never
    // actually earned.
    let landed = client.manifest(destination)?;
    if landed != *manifest {
        return Err(RpcError::Remote(OpError::Internal));
    }
    client.set_mtime(destination, crate::state::now_unix_secs())?;
    Ok(())
}

/// True when `name`, a path's last segment, is this bridge's own partial
/// marker. `docs/engine-contract.md`, item 6, I2: "a `.ferry-part` name is
/// 404 on `GET`, `HEAD`, and `PROPFIND`, and never listed."
#[must_use]
pub(crate) fn is_partial_name(name: &str) -> bool {
    name.ends_with(".ferry-part")
}

#[cfg(test)]
mod tests {
    use std::thread;

    use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
    use ferry_core::memfs::MemoryFs;
    use ferry_core::path::RemotePath;
    use ferry_core::rpc::{Client, FileOps, RpcError, serve};
    use ferry_core::transport::{Endpoint, loopback};

    use super::{land_delta, land_new};

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    fn spawn_server(
        fs: impl FileOps + 'static,
    ) -> (Client<Endpoint>, thread::JoinHandle<Result<(), RpcError>>) {
        let (client_end, mut server_end) = loopback();
        let handle = thread::spawn(move || serve(&mut server_end, &fs));
        (Client::new(client_end), handle)
    }

    fn finish(client: Client<Endpoint>, handle: thread::JoinHandle<Result<(), RpcError>>) {
        drop(client);
        assert!(handle.join().unwrap().is_ok());
    }

    /// A temporary folder holding one file, whose bytes a test spools
    /// through [`super::open_spool`].
    struct SpoolFixture {
        dir: std::path::PathBuf,
    }

    impl SpoolFixture {
        fn new(label: &str, bytes: &[u8]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ferry-dav-put-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("spool"), bytes).unwrap();
            Self { dir }
        }

        fn spool_path(&self) -> std::path::PathBuf {
            self.dir.join("spool")
        }
    }

    impl Drop for SpoolFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn land_new_lands_a_file_the_peer_never_had() {
        let fixture = SpoolFixture::new("new", b"hello from the spool");
        let (fs, leaf, manifest) = super::open_spool(&fixture.spool_path()).unwrap();

        let peer = MemoryFs::new();
        let (mut client, handle) = spawn_server(peer);
        let mut bytes_written = 0u64;
        land_new(
            &mut client,
            &path("Root/a.txt"),
            &fs,
            &leaf,
            &manifest,
            &mut bytes_written,
        )
        .unwrap();
        assert_eq!(bytes_written, 20, "every byte of the file was sent");

        let landed = client.read(&path("Root/a.txt"), 0, 64).unwrap();
        assert_eq!(landed, b"hello from the spool");
        // No `.ferry-part` should survive a landing that succeeded.
        assert!(client.stat(&path("Root/a.txt.ferry-part")).is_err());

        finish(client, handle);
    }

    #[test]
    fn land_delta_writes_only_the_chunk_that_changed() {
        let chunk_size = ChunkSize::one_mebibyte();
        let mut original = vec![0u8; chunk_size.as_usize() * 2];
        original[0] = 1;
        original[chunk_size.as_usize()] = 2;
        let mut changed = original.clone();
        changed[chunk_size.as_usize()] = 99; // only the second chunk differs

        let peer = MemoryFs::new();
        peer.insert_file("Root/a.txt", original);
        let (mut client, handle) = spawn_server(peer);

        let fixture = SpoolFixture::new("delta", &changed);
        let (fs, leaf, manifest) = super::open_spool(&fixture.spool_path()).unwrap();
        let mut bytes_written = 0u64;
        land_delta(
            &mut client,
            &path("Root/a.txt"),
            &fs,
            &leaf,
            &manifest,
            &mut bytes_written,
        )
        .unwrap();
        assert_eq!(
            bytes_written,
            chunk_size.as_u64(),
            "only the one changed chunk should be sent, not the whole file"
        );

        // One `read` call is bounded to `MAX_READ_LEN` (one mebibyte), so
        // the two chunks are fetched back separately.
        let mut landed = client
            .read(
                &path("Root/a.txt"),
                0,
                u32::try_from(chunk_size.as_usize()).unwrap(),
            )
            .unwrap();
        landed.extend(
            client
                .read(
                    &path("Root/a.txt"),
                    chunk_size.as_u64(),
                    u32::try_from(chunk_size.as_usize()).unwrap(),
                )
                .unwrap(),
        );
        assert_eq!(landed, changed);

        finish(client, handle);
    }

    #[test]
    fn land_delta_shrinks_a_file_that_got_shorter() {
        let peer = MemoryFs::new();
        peer.insert_file("Root/a.txt", b"a long original file".to_vec());
        let (mut client, handle) = spawn_server(peer);

        let fixture = SpoolFixture::new("shrink", b"short");
        let (fs, leaf, manifest) = super::open_spool(&fixture.spool_path()).unwrap();
        let mut bytes_written = 0u64;
        land_delta(
            &mut client,
            &path("Root/a.txt"),
            &fs,
            &leaf,
            &manifest,
            &mut bytes_written,
        )
        .unwrap();

        let entry = client.stat(&path("Root/a.txt")).unwrap();
        assert_eq!(entry.size, 5);
        let landed = client.read(&path("Root/a.txt"), 0, 5).unwrap();
        assert_eq!(landed, b"short");

        finish(client, handle);
    }

    /// Wraps a real [`MemoryFs`], answering the first `manifest` call
    /// honestly and every one after that only once it has appended one
    /// byte directly to the file, bypassing whatever `land_delta` itself
    /// wrote. This stands in for a peer whose file changes between
    /// `land_delta`'s opening manifest read and the writes that follow it,
    /// without needing to race two threads against each other.
    struct ChangesAfterFirstManifest {
        inner: MemoryFs,
        manifest_calls: std::sync::atomic::AtomicUsize,
    }

    impl ferry_core::rpc::FileOps for ChangesAfterFirstManifest {
        fn manifest(
            &self,
            path: &RemotePath,
        ) -> Result<ferry_core::chunk::Manifest, ferry_core::ops::OpError> {
            let n = self
                .manifest_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 1
                && let Some(mut bytes) = self.inner.file_bytes(path.as_str())
            {
                bytes.push(b'!');
                self.inner.insert_file(path.as_str(), bytes);
            }
            self.inner.manifest(path)
        }
        fn list(
            &self,
            path: &RemotePath,
            cursor: u64,
        ) -> Result<(Vec<ferry_core::ops::Entry>, Option<u64>), ferry_core::ops::OpError> {
            self.inner.list(path, cursor)
        }
        fn stat(
            &self,
            path: &RemotePath,
        ) -> Result<ferry_core::ops::Entry, ferry_core::ops::OpError> {
            self.inner.stat(path)
        }
        fn read(
            &self,
            path: &RemotePath,
            offset: u64,
            length: u32,
        ) -> Result<Vec<u8>, ferry_core::ops::OpError> {
            self.inner.read(path, offset, length)
        }
        fn write(
            &self,
            path: &RemotePath,
            offset: u64,
            bytes: &[u8],
        ) -> Result<u32, ferry_core::ops::OpError> {
            self.inner.write(path, offset, bytes)
        }
        fn truncate(&self, path: &RemotePath, length: u64) -> Result<(), ferry_core::ops::OpError> {
            self.inner.truncate(path, length)
        }
        fn rename(
            &self,
            from: &RemotePath,
            to: &RemotePath,
        ) -> Result<(), ferry_core::ops::OpError> {
            self.inner.rename(from, to)
        }
        fn set_mtime(
            &self,
            path: &RemotePath,
            modified_unix_secs: i64,
        ) -> Result<(), ferry_core::ops::OpError> {
            self.inner.set_mtime(path, modified_unix_secs)
        }
        fn mkdir(&self, path: &RemotePath) -> Result<(), ferry_core::ops::OpError> {
            self.inner.mkdir(path)
        }
        fn delete(&self, path: &RemotePath) -> Result<(), ferry_core::ops::OpError> {
            self.inner.delete(path)
        }
    }

    #[test]
    fn land_delta_keeps_the_old_modified_time_when_the_peer_changes_first() {
        let peer = MemoryFs::new();
        peer.insert_file("Root/a.txt", b"a long original file".to_vec());
        let peer = ChangesAfterFirstManifest {
            inner: peer,
            manifest_calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let (mut client, handle) = spawn_server(peer);

        let fixture = SpoolFixture::new("mid-change", b"short");
        let (fs, leaf, manifest) = super::open_spool(&fixture.spool_path()).unwrap();
        let mut bytes_written = 0u64;
        let error = land_delta(
            &mut client,
            &path("Root/a.txt"),
            &fs,
            &leaf,
            &manifest,
            &mut bytes_written,
        )
        .expect_err("the peer's file changed underneath the landing");
        assert!(matches!(
            error,
            RpcError::Remote(ferry_core::ops::OpError::Internal)
        ));
        assert_eq!(
            bytes_written, 5,
            "a failed landing still reports what it actually wrote"
        );

        // `ChangesAfterFirstManifest` itself writes the file directly
        // (bypassing `land_delta`'s own writes) to simulate the peer
        // changing underneath the landing, and that direct write resets
        // the modified time to 0. A `land_delta` that still called
        // `set_mtime` after its manifest check failed would show a recent,
        // real timestamp here instead.
        let entry = client.stat(&path("Root/a.txt")).unwrap();
        assert_eq!(
            entry.modified_unix_secs, 0,
            "a failed landing must never touch the modified time"
        );

        finish(client, handle);
    }

    #[test]
    fn open_spool_reads_back_the_manifest_a_manifest_builder_would() {
        let fixture = SpoolFixture::new("manifest", b"0123456789");
        let (_fs, _leaf, manifest) = super::open_spool(&fixture.spool_path()).unwrap();
        assert_eq!(
            manifest,
            manifest_from_bytes(b"0123456789", ChunkSize::one_mebibyte())
        );
    }
}
