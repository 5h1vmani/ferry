//! Transfers that outlive the connection that started them.
//!
//! A transfer holds an identifier that does not belong to any connection. Both
//! sides keep a manifest. When the Wi-Fi drops or a cable is pulled, a new
//! connection continues the same transfer.
//!
//! Ferry never migrates a live stream between transports. It resumes a session
//! instead. See decision record 5.
//!
//! # The manifest is a hint, the disk is the truth
//!
//! Resume never believes a stored record about which chunks arrived. It reads
//! the partial file back and hashes it. BLAKE3 runs at over one gigabyte per
//! second, so this costs little and it removes a whole class of quiet
//! corruption.
//!
//! # Nothing lands at the real name until it is whole
//!
//! Incoming bytes go to a temporary name beside the destination. The file is
//! renamed only after every chunk has verified. Without that, Android's gallery
//! shows half written videos, and an interrupted transfer leaves a broken file
//! where the real one should be.

use std::fmt;
use std::io::{Read, Write};

use crate::chunk::{Manifest, ManifestError};
use crate::limits;
use crate::ops::OpError;
use crate::path::{PathError, RemotePath};
use crate::rpc::{Client, FileOps, RpcError};
use crate::wire::{Decoder, Encoder, WireError};

/// Names one transfer, across as many connections as it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// Create a new random identifier.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::NoRandomness`] when the system cannot supply
    /// random bytes.
    pub fn generate() -> Result<Self, TransferError> {
        let mut out = [0u8; 16];
        getrandom::fill(&mut out).map_err(|_| TransferError::NoRandomness)?;
        Ok(Self(out))
    }

    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The reason a transfer stopped.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The connection or the peer failed.
    #[error("the connection failed: {0}")]
    Rpc(#[from] RpcError),
    /// The local filesystem refused something.
    #[error("local storage refused: {0}")]
    Local(#[from] OpError),
    /// A stored transfer record was malformed.
    #[error("the stored transfer is unusable: {0}")]
    Record(#[from] ManifestError),
    /// A stored transfer record held an unusable path.
    #[error("the stored transfer holds a bad path: {0}")]
    BadPath(#[from] PathError),
    /// A chunk arrived, but its bytes do not match the manifest.
    ///
    /// The sender is faulty, or the file changed under it. Either way the
    /// transfer stops rather than writing bytes that will not verify.
    #[error("chunk {index} did not match the manifest")]
    ChunkFailedVerification {
        /// Which chunk failed.
        index: usize,
    },
    /// The peer returned fewer bytes than the range holds.
    #[error("chunk {index} needed {wanted} bytes but {got} arrived")]
    ShortRead {
        /// Which chunk was being fetched.
        index: usize,
        /// How many bytes the manifest says the chunk holds.
        wanted: u32,
        /// How many arrived.
        got: usize,
    },
    /// The system could not supply random bytes.
    #[error("the system supplied no random bytes")]
    NoRandomness,
}

impl From<WireError> for TransferError {
    fn from(value: WireError) -> Self {
        Self::Record(ManifestError::Wire(value))
    }
}

/// How far a transfer has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// How many chunks have been verified in place.
    pub chunks_done: usize,
    /// How many chunks the file holds in total.
    pub chunks_total: usize,
    /// How many bytes are verified in place.
    pub bytes_done: u64,
}

impl Progress {
    /// True when every chunk is verified in place.
    #[must_use]
    pub fn is_complete(self) -> bool {
        self.chunks_done == self.chunks_total
    }
}

/// Everything needed to continue a transfer after a restart.
///
/// Store this next to the partial file. It carries no connection state, which
/// is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    /// Names this transfer across connections.
    pub id: SessionId,
    /// What the file should look like when it is whole.
    pub manifest: Manifest,
    /// Where the file lives on the sending device.
    pub source: RemotePath,
    /// Where the file belongs on this device, once it verifies.
    pub destination: RemotePath,
}

impl Transfer {
    /// Start a new transfer.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::NoRandomness`] when no identifier can be
    /// made, and [`TransferError::BadPath`] when the source or the
    /// destination is the shared root. A transfer always moves one named
    /// file. The root has no file of its own to move.
    pub fn new(
        manifest: Manifest,
        source: RemotePath,
        destination: RemotePath,
    ) -> Result<Self, TransferError> {
        if source.is_root() || destination.is_root() {
            return Err(PathError::Empty.into());
        }
        Ok(Self {
            id: SessionId::generate()?,
            manifest,
            source,
            destination,
        })
    }

    /// Where incoming bytes are written until the file verifies.
    ///
    /// The identifier is part of the name, so two transfers of the same file
    /// never write over each other.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::BadPath`] when the resulting name is too long
    /// for a remote path.
    pub fn temporary_path(&self) -> Result<RemotePath, TransferError> {
        let name = format!("{}.{}.part", self.destination.as_str(), self.id);
        Ok(RemotePath::parse(&name)?)
    }

    /// Write the transfer record out, for storage beside the partial file.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.fixed(self.id.as_bytes());
        e.text(self.source.as_str());
        e.text(self.destination.as_str());
        e.bytes(&self.manifest.encode());
        e.finish()
    }

    /// Read a transfer record back.
    ///
    /// The manifest inside is checked, so a record edited on disk is refused.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::Record`] when the bytes are malformed or the
    /// manifest does not agree with itself.
    pub fn decode(bytes: &[u8]) -> Result<Self, TransferError> {
        let mut d = Decoder::new(bytes);
        let id = SessionId(d.fixed::<16>()?);
        let source = RemotePath::parse(d.text(limits::MAX_PATH_LEN)?)?;
        let destination = RemotePath::parse(d.text(limits::MAX_PATH_LEN)?)?;
        let manifest = Manifest::decode(d.bytes(limits::MAX_MANIFEST_BYTES)?)?;
        d.finish()?;
        Ok(Self {
            id,
            manifest,
            source,
            destination,
        })
    }
}

/// Read a whole byte range from a source that only serves limited pieces.
///
/// Both local storage and a remote peer cap a single read, so both need the
/// same loop. Writing it twice invites the two copies to drift.
///
/// `fetch` is called with an offset relative to the start of the range. It
/// returns an empty result when the source has no more bytes, which ends the
/// loop and leaves the caller to decide whether a short result is a problem.
///
/// A read is also short when [`limits::MAX_READS_PER_CHUNK`] reads have run
/// and the range is still not full. That cap exists so that a peer which
/// answers a byte or two per read cannot hold this loop for as many round
/// trips as the range has bytes. What has been read so far is returned, as
/// the same kind of short result an empty answer produces, for the caller to
/// judge.
fn read_range<E>(
    length: u32,
    mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
) -> Result<Vec<u8>, E> {
    let mut out: Vec<u8> = Vec::with_capacity(length as usize);
    let mut reads: u32 = 0;
    loop {
        let done = u32::try_from(out.len()).unwrap_or(length);
        if done >= length {
            return Ok(out);
        }
        if reads >= limits::MAX_READS_PER_CHUNK {
            return Ok(out);
        }
        let piece = (length - done).min(limits::MAX_READ_LEN);
        let got = fetch(u64::from(done), piece)?;
        reads += 1;
        if got.is_empty() {
            return Ok(out);
        }
        out.extend_from_slice(&got);
    }
}

/// Read one byte range from local storage.
///
/// A short result means the partial file does not reach that far yet.
fn read_local(
    local: &dyn FileOps,
    path: &RemotePath,
    offset: u64,
    length: u32,
) -> Result<Vec<u8>, OpError> {
    read_range(length, |at, piece| local.read(path, offset + at, piece))
}

/// Write one byte range to local storage, in pieces the trait allows.
fn write_local(
    local: &dyn FileOps,
    path: &RemotePath,
    offset: u64,
    bytes: &[u8],
) -> Result<(), OpError> {
    let mut written = 0usize;
    while written < bytes.len() {
        let piece = (bytes.len() - written).min(limits::MAX_WRITE_LEN as usize);
        let n = local.write(
            path,
            offset + written as u64,
            &bytes[written..written + piece],
        )?;
        if n == 0 {
            return Err(OpError::Internal);
        }
        written += n as usize;
    }
    Ok(())
}

/// Fetch one whole chunk from the peer.
///
/// A chunk may be larger than one read allows, so this may take several calls.
fn fetch_chunk<S: Read + Write>(
    client: &mut Client<S>,
    source: &RemotePath,
    index: usize,
    offset: u64,
    length: u32,
) -> Result<Vec<u8>, TransferError> {
    let got = read_range(length, |at, piece| client.read(source, offset + at, piece))?;
    // A chunk inside the file is always whole. Anything shorter means the peer
    // no longer holds the file the manifest describes.
    if got.len() != length as usize {
        return Err(TransferError::ShortRead {
            index,
            wanted: length,
            got: got.len(),
        });
    }
    Ok(got)
}

/// Find the first chunk the partial file does not already hold.
///
/// Every chunk before the answer has been read back from disk and hashed. No
/// stored claim about progress is believed.
///
/// # Errors
///
/// Returns [`TransferError::Local`] when local storage fails for a reason other
/// than the partial file being absent.
pub fn resume_point(
    local: &dyn FileOps,
    temporary: &RemotePath,
    manifest: &Manifest,
) -> Result<usize, TransferError> {
    match local.stat(temporary) {
        Ok(_) => {}
        // Nothing has arrived yet, so start at the beginning.
        Err(OpError::NotFound) => return Ok(0),
        Err(other) => return Err(other.into()),
    }

    for index in 0..manifest.chunk_count() {
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = read_local(local, temporary, offset, length)?;
        if !manifest.verify_chunk(index, &bytes) {
            return Ok(index);
        }
    }
    Ok(manifest.chunk_count())
}

/// Pull a file from the peer into local storage, starting or resuming.
///
/// Call this again on a new connection after a failure. It works out where to
/// continue by reading the partial file back.
///
/// On success the file has been verified chunk by chunk, cut to its exact
/// length, and renamed from the temporary name to the destination.
///
/// # Errors
///
/// Returns [`TransferError::Rpc`] when the connection fails, which is the
/// ordinary interruption this function is built for. Returns
/// [`TransferError::ChunkFailedVerification`] when the peer sends bytes that do
/// not match the manifest.
pub fn pull<S: Read + Write>(
    client: &mut Client<S>,
    transfer: &Transfer,
    local: &dyn FileOps,
) -> Result<Progress, TransferError> {
    pull_with_progress(client, transfer, local, |_| {})
}

/// Pull a file, and report after every chunk that verifies.
///
/// This is [`pull`] with one extra argument. `on_chunk` is called once per
/// chunk, after the chunk has verified and reached the disk. It is never
/// called for a chunk that a resume skipped, because those bytes moved on an
/// earlier connection.
///
/// The engine uses this to drive a progress bar. `pull` is the same function
/// with a callback that does nothing.
///
/// # Errors
///
/// As [`pull`].
pub fn pull_with_progress<S: Read + Write>(
    client: &mut Client<S>,
    transfer: &Transfer,
    local: &dyn FileOps,
    mut on_chunk: impl FnMut(Progress),
) -> Result<Progress, TransferError> {
    let manifest = &transfer.manifest;
    let temporary = transfer.temporary_path()?;
    let start = resume_point(local, &temporary, manifest)?;

    for index in start..manifest.chunk_count() {
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = fetch_chunk(client, &transfer.source, index, offset, length)?;
        // Verify before writing, so nothing that fails ever reaches the disk.
        if !manifest.verify_chunk(index, &bytes) {
            return Err(TransferError::ChunkFailedVerification { index });
        }
        write_local(local, &temporary, offset, &bytes)?;
        on_chunk(Progress {
            chunks_done: index + 1,
            chunks_total: manifest.chunk_count(),
            bytes_done: offset + u64::from(length),
        });
    }

    // A resumed file may be longer than the manifest if an earlier attempt
    // wrote past the end. Cut it back before it takes the real name.
    local.truncate(&temporary, manifest.length())?;

    // Every chunk verified, and the manifest's own chunks were checked against
    // its root hash when it was decoded. So the whole file is correct without
    // a second pass over the bytes.
    local.rename(&temporary, &transfer.destination)?;

    Ok(Progress {
        chunks_done: manifest.chunk_count(),
        chunks_total: manifest.chunk_count(),
        bytes_done: manifest.length(),
    })
}

#[cfg(test)]
mod tests {
    use super::{Transfer, TransferError, pull, read_range, resume_point};
    use crate::chunk::{ChunkSize, manifest_from_bytes};
    use crate::memfs::MemoryFs;
    use crate::path::{PathError, RemotePath};
    use crate::rpc::{Client, serve};
    use crate::transport::{Endpoint, loopback};
    use std::sync::Arc;

    const SOURCE: &str = "DCIM/Camera/VID.mp4";
    const DESTINATION: &str = "Movies/VID.mp4";

    fn data(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from((i * 7) % 251).unwrap())
            .collect()
    }

    fn remote_with(bytes: &[u8]) -> Arc<MemoryFs> {
        let fs = Arc::new(MemoryFs::new());
        fs.insert_file(SOURCE, bytes.to_vec());
        fs
    }

    fn local_empty() -> Arc<MemoryFs> {
        let fs = Arc::new(MemoryFs::new());
        fs.insert_dir("Movies");
        fs
    }

    /// Serve `remote` on one endpoint and hand back a client on the other.
    ///
    /// The caller may cap how many bytes the server can send, which is how an
    /// interruption is placed at a chosen point.
    fn connect(
        remote: Arc<MemoryFs>,
        server_byte_budget: Option<u64>,
    ) -> (Client<Endpoint>, std::thread::JoinHandle<()>) {
        let (client_end, mut server_end) = loopback();
        if let Some(budget) = server_byte_budget {
            server_end.break_after_sending(budget);
        }
        let handle = std::thread::spawn(move || {
            let _ = serve(&mut server_end, remote.as_ref());
        });
        (Client::new(client_end), handle)
    }

    fn transfer_for(bytes: &[u8]) -> Transfer {
        let manifest = manifest_from_bytes(bytes, ChunkSize::new(1024).unwrap());
        Transfer::new(
            manifest,
            RemotePath::parse(SOURCE).unwrap(),
            RemotePath::parse(DESTINATION).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_whole_transfer_lands_at_the_destination() {
        let bytes = data(5000);
        let local = local_empty();
        let transfer = transfer_for(&bytes);
        let (mut client, server) = connect(remote_with(&bytes), None);

        let progress = pull(&mut client, &transfer, local.as_ref()).unwrap();
        assert!(progress.is_complete());
        assert_eq!(progress.bytes_done, 5000);
        assert_eq!(local.file_bytes(DESTINATION), Some(bytes));

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn the_temporary_file_is_gone_once_the_file_verifies() {
        let bytes = data(5000);
        let local = local_empty();
        let transfer = transfer_for(&bytes);
        let (mut client, server) = connect(remote_with(&bytes), None);
        pull(&mut client, &transfer, local.as_ref()).unwrap();

        let temporary = transfer.temporary_path().unwrap();
        assert!(local.file_bytes(temporary.as_str()).is_none());

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn an_interrupted_transfer_resumes_on_a_new_connection() {
        // This is the reliability claim, tested end to end. The link dies part
        // way through, and a fresh connection finishes the same transfer.
        let bytes = data(5000);
        let local = local_empty();
        let transfer = transfer_for(&bytes);

        let (mut client, server) = connect(remote_with(&bytes), Some(2500));
        let first = pull(&mut client, &transfer, local.as_ref());
        assert!(first.is_err(), "the transfer must fail when the link dies");
        assert!(
            local.file_bytes(DESTINATION).is_none(),
            "nothing lands early"
        );
        drop(client);
        server.join().unwrap();

        // Some chunks survived, so the second attempt must not start at zero.
        let temporary = transfer.temporary_path().unwrap();
        let point = resume_point(local.as_ref(), &temporary, &transfer.manifest).unwrap();
        assert!(point > 0, "at least one chunk should have arrived");
        assert!(
            point < transfer.manifest.chunk_count(),
            "the transfer was not finished"
        );

        let (mut client, server) = connect(remote_with(&bytes), None);
        let second = pull(&mut client, &transfer, local.as_ref()).unwrap();
        assert!(second.is_complete());
        assert_eq!(local.file_bytes(DESTINATION), Some(bytes));

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn resume_reads_the_disk_rather_than_trusting_a_record() {
        // A partial file whose bytes were edited must be refetched, even though
        // no stored record says anything is wrong.
        let bytes = data(5000);
        let local = local_empty();
        let transfer = transfer_for(&bytes);
        let temporary = transfer.temporary_path().unwrap();

        // Pretend the first two chunks arrived, then corrupt the second.
        let mut partial = bytes[..2048].to_vec();
        partial[1500] ^= 0xFF;
        local.insert_file(temporary.as_str(), partial);

        let point = resume_point(local.as_ref(), &temporary, &transfer.manifest).unwrap();
        assert_eq!(point, 1, "the damaged chunk must be fetched again");

        let (mut client, server) = connect(remote_with(&bytes), None);
        pull(&mut client, &transfer, local.as_ref()).unwrap();
        assert_eq!(local.file_bytes(DESTINATION), Some(bytes));

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn a_peer_that_sends_the_wrong_bytes_is_caught() {
        // The manifest describes one file, the peer holds another. Nothing
        // that fails verification is ever written.
        let wanted = data(5000);
        let wrong_bytes = data(5000).iter().map(|b| b ^ 0x5A).collect::<Vec<u8>>();
        let local = local_empty();
        let transfer = transfer_for(&wanted);
        let (mut client, server) = connect(remote_with(&wrong_bytes), None);

        match pull(&mut client, &transfer, local.as_ref()) {
            Err(TransferError::ChunkFailedVerification { index }) => assert_eq!(index, 0),
            other => panic!("expected a verification failure, got {other:?}"),
        }
        assert!(local.file_bytes(DESTINATION).is_none());

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn a_short_file_that_fits_one_chunk_transfers() {
        let bytes = data(200);
        let local = local_empty();
        let transfer = transfer_for(&bytes);
        let (mut client, server) = connect(remote_with(&bytes), None);
        pull(&mut client, &transfer, local.as_ref()).unwrap();
        assert_eq!(local.file_bytes(DESTINATION), Some(bytes));
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn a_transfer_record_survives_being_stored_and_read_back() {
        let transfer = transfer_for(&data(5000));
        let restored = Transfer::decode(&transfer.encode()).unwrap();
        assert_eq!(restored, transfer);
    }

    #[test]
    fn two_transfers_of_one_file_use_different_temporary_names() {
        // Otherwise a second attempt would write over the first one's bytes.
        let bytes = data(5000);
        let a = transfer_for(&bytes);
        let b = transfer_for(&bytes);
        assert_ne!(a.id, b.id);
        assert_ne!(a.temporary_path().unwrap(), b.temporary_path().unwrap());
    }

    #[test]
    fn a_transfer_cannot_use_the_root_as_its_source() {
        let manifest = manifest_from_bytes(&data(10), ChunkSize::new(1024).unwrap());
        let error = Transfer::new(
            manifest,
            RemotePath::parse("").unwrap(),
            RemotePath::parse(DESTINATION).unwrap(),
        );
        assert!(matches!(
            error,
            Err(TransferError::BadPath(PathError::Empty))
        ));
    }

    #[test]
    fn a_transfer_cannot_use_the_root_as_its_destination() {
        let manifest = manifest_from_bytes(&data(10), ChunkSize::new(1024).unwrap());
        let error = Transfer::new(
            manifest,
            RemotePath::parse(SOURCE).unwrap(),
            RemotePath::parse("").unwrap(),
        );
        assert!(matches!(
            error,
            Err(TransferError::BadPath(PathError::Empty))
        ));
    }

    #[test]
    fn read_range_gives_up_after_the_read_cap_and_returns_what_it_has() {
        // A peer that answers one byte per read must not hold this loop for
        // as many reads as the range has bytes. The range asked for here
        // needs far more than the cap allows at one byte per read, so only a
        // working cap ends the loop.
        let mut calls: u32 = 0;
        let result: Result<Vec<u8>, TransferError> = read_range(10_000, |_at, _piece| {
            calls += 1;
            Ok(vec![7u8])
        });
        let got = result.unwrap();
        assert_eq!(calls, crate::limits::MAX_READS_PER_CHUNK);
        assert_eq!(got.len(), crate::limits::MAX_READS_PER_CHUNK as usize);
    }
}
