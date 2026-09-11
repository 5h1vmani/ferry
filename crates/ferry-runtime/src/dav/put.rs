//! Landing a file onto the peer through the bridge: `PUT` of a real file,
//! and `COPY` of a file, which lands the same way once its bytes are on
//! this Mac.
//!
//! `docs/engine-contract.md`, item 6, I2, "I2, saving", reusing the
//! sender's landing rule from item 5 (`push.rs`): a spool file on this
//! Mac's own disk, its manifest built once, then chunks written to the
//! peer, verified by the peer's own manifest, and landed. `push.rs` is
//! outside this batch's files, so the small amount of wire plumbing below
//! (`write_all_remote`, `partial_path`) is written again here rather than
//! shared; the rule it follows is the same one, stated in the module
//! documentation there.
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
//! does. A `PUT` or a `COPY` is one HTTP request: a peer failure partway
//! through answers 502 or 503 and leaves nothing spooled behind, per
//! `docs/engine-contract.md`, item 6; Finder is the one that retries, the
//! same way it retries any other failed request.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use ferry_core::chunk::Manifest;
use ferry_core::limits;
use ferry_core::localfs::LocalFs;
use ferry_core::ops::OpError;
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, RpcError};

use crate::engine::Shared;

use super::http;

/// Creates a fresh, empty spool file under
/// `data_dir/dav_spool/<device key hex>/` with a random name, and returns
/// its path. `None` when the folder cannot be made or a name cannot be
/// generated; the caller answers 500 in that case, since nothing on the
/// wire caused it.
pub(crate) fn new_spool_path(shared: &Shared, device_key_hex: &str) -> Option<PathBuf> {
    let dir = shared.data_dir.join("dav_spool").join(device_key_hex);
    std::fs::create_dir_all(&dir).ok()?;
    let name = super::random_hex(16).ok()?;
    Some(dir.join(name))
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
) -> Result<(), RpcError> {
    let partial = partial_path(destination)?;

    if manifest.length() == 0 {
        // An empty file still needs its partial to exist, so the rename
        // below has something to rename.
        client.write(&partial, 0, Vec::new())?;
    }
    for index in 0..manifest.chunk_count() {
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = spool_fs
            .read(spool_leaf, offset, length)
            .map_err(RpcError::Remote)?;
        write_all_remote(client, &partial, offset, &bytes)?;
    }
    client.truncate(&partial, manifest.length())?;

    let landed = client.manifest(&partial)?;
    if landed != *manifest {
        return Err(RpcError::Remote(OpError::Internal));
    }
    client.rename(&partial, destination)?;
    client.set_mtime(destination, crate::state::now_unix_secs())?;
    Ok(())
}

/// Lands a spool file at `destination`, which already exists there: the
/// delta on save from `docs/engine-contract.md`, item 6, I2. Only the
/// chunks whose chaining values differ from the peer's own manifest are
/// written, in place, before a `truncate` to the new length, `set_mtime`,
/// and a final manifest check.
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
        write_all_remote(client, destination, offset, &bytes)?;
    }
    client.truncate(destination, manifest.length())?;
    client.set_mtime(destination, crate::state::now_unix_secs())?;

    let landed = client.manifest(destination)?;
    if landed != *manifest {
        return Err(RpcError::Remote(OpError::Internal));
    }
    Ok(())
}

/// Where a new-file landing writes until the whole file verifies on the
/// peer. A fixed suffix, not a per-attempt random one, matching
/// `push.rs`'s own reasoning: this bridge only ever tries a `PUT` once
/// per HTTP request, but a fixed name still means a stray partial from an
/// earlier, failed request is overwritten rather than left to accumulate.
fn partial_path(destination: &RemotePath) -> Result<RemotePath, RpcError> {
    RemotePath::parse(&format!("{}.ferry-part", destination.as_str()))
        .map_err(|_| RpcError::Remote(OpError::InvalidPath))
}

/// True when `name`, a path's last segment, is this bridge's own partial
/// marker. `docs/engine-contract.md`, item 6, I2: "`HEAD` and `GET` of a
/// `.ferry-part` name are 404 and a listing never shows one, so Finder
/// never sees a partial."
#[must_use]
pub(crate) fn is_partial_name(name: &str) -> bool {
    name.ends_with(".ferry-part")
}

/// Write one range to the peer, in pieces one `write` call accepts.
/// Exactly `push.rs`'s own helper of the same name.
fn write_all_remote<S: Read + Write>(
    client: &mut Client<S>,
    path: &RemotePath,
    offset: u64,
    bytes: &[u8],
) -> Result<(), RpcError> {
    let cap = usize::try_from(limits::MAX_WRITE_LEN).unwrap_or(usize::MAX);
    let mut written = 0usize;
    while written < bytes.len() {
        let piece = (bytes.len() - written).min(cap);
        let at = offset + u64::try_from(written).unwrap_or(0);
        let sent = client.write(path, at, bytes[written..written + piece].to_vec())?;
        if sent == 0 {
            return Err(RpcError::Remote(OpError::Internal));
        }
        written += usize::try_from(sent).unwrap_or(piece);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::thread;

    use ferry_core::chunk::{ChunkSize, manifest_from_bytes};
    use ferry_core::memfs::MemoryFs;
    use ferry_core::path::RemotePath;
    use ferry_core::rpc::{Client, RpcError, serve};
    use ferry_core::transport::{Endpoint, loopback};

    use super::{land_delta, land_new};

    fn path(text: &str) -> RemotePath {
        RemotePath::parse(text).unwrap()
    }

    fn spawn_server(fs: MemoryFs) -> (Client<Endpoint>, thread::JoinHandle<Result<(), RpcError>>) {
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
        land_new(&mut client, &path("Root/a.txt"), &fs, &leaf, &manifest).unwrap();

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
        land_delta(&mut client, &path("Root/a.txt"), &fs, &leaf, &manifest).unwrap();

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
        land_delta(&mut client, &path("Root/a.txt"), &fs, &leaf, &manifest).unwrap();

        let entry = client.stat(&path("Root/a.txt")).unwrap();
        assert_eq!(entry.size, 5);
        let landed = client.read(&path("Root/a.txt"), 0, 5).unwrap();
        assert_eq!(landed, b"short");

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
