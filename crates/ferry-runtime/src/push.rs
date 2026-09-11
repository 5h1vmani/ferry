//! Sending a file to a peer. `docs/engine-contract.md` item 5.
//!
//! Every pull request in this engine is a `read`. A push is the mirror: a
//! loop of `write` calls, driven entirely by the sender. The peer serves
//! `write`, `truncate`, `rename`, `set_mtime`, and the manifest request
//! (item 16a), and its access log records the writes the same way it
//! records any other peer's. Everything that decides what happens next
//! happens here, on the sending side.
//!
//! # Reusing the pull's plumbing
//!
//! The worker pool, the queue, the backoff, [`crate::transfer::Plan`], and
//! the transfer record format in `record.rs` are all direction neutral
//! already: `Plan` and `Meta` both carry a [`Direction`], and `Record`
//! stores whichever way a transfer moves. `crate::transfer::attempt`
//! dispatches to this module once dialling and the `hello` exchange are
//! done, and hands this module a live client on that connection.
//!
//! `TransferRow::source` and `TransferRow::destination` are both
//! [`RemotePath`], a type built for a path that is relative to a named
//! root. A pull's `destination` already is one: it is relative to the
//! download folder. A push's local path has no such root; it is absolute
//! on this device. Rather than widen a type every other transfer depends
//! on, this module treats the local filesystem root, `/`, as that missing
//! root: [`store_local_path`] strips the leading slash to store the path,
//! and [`local_absolute_path`] undoes it. The checks `RemotePath::parse`
//! already runs (no NUL byte, no backslash, no `..` component, a bounded
//! length) are exactly the checks a local path deserves too, so nothing is
//! lost by sharing the type.
//!
//! # Why there is no first pass
//!
//! A pull's first pass exists because fetching the peer's manifest and its
//! first bytes both need the network, so item 16a's design fetches the
//! manifest once and verifies chunks as they arrive in the same pass,
//! rather than paying for a second round trip. A push's manifest describes
//! *this* device's own file, which [`ferry_core::localfs::LocalFs::manifest`]
//! builds from local disk with no network at all. So the manifest is built
//! once, up front, and stored in a [`crate::record::Record::Ready`] from
//! the very first attempt; there is no local equivalent of `FirstPass` to
//! resume through.
//!
//! # Resume
//!
//! `docs/protocol.md` section 9 ("the manifest is a hint, the disk is the
//! truth") is written for a puller re-hashing its own local disk. A pusher
//! cannot do that: the disk in question is the peer's. So a push asks the
//! peer for the manifest of `<remote_path>.ferry-part` instead, and
//! compares chaining values against its own manifest to find the first
//! chunk that differs ([`first_mismatch`]). That happens at the start of
//! every attempt, and once more after every chunk has been sent, per item
//! 5: a matching root on a matching length lands the file; anything else
//! is worth another attempt, which repeats the same comparison and sends
//! only what still differs.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use ferry_core::chunk::Manifest;
use ferry_core::limits;
use ferry_core::localfs::LocalFs;
use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::{PathError, RemotePath};
use ferry_core::rpc::{Client, FileOps, RpcError, exchange_hello};
use ferry_core::session::{SessionId, Transfer, TransferError};

use crate::access;
use crate::batch::{self, BatchRecord};
use crate::engine::{self, Shared};
use crate::errors::{failed, from_op, from_path, from_rpc, from_transfer};
use crate::guard::StopAware;
use crate::notify::Change;
use crate::record::{Meta, Record, read_record, write_record};
use crate::state::{TransferRow, key_from_hex, lock, now_unix_secs};
use crate::transfer::{self, BACKOFF_MIN, Outcome, Plan, Reporter, classify_rpc};
use crate::{Direction, FerryError, Origin, TransferState};

// ---------------------------------------------------------------------------
// The boundary: `Engine::push` and `Engine::push_files` call straight in.
// ---------------------------------------------------------------------------

/// Send one file. See `Engine::push` in `engine.rs`.
pub(crate) fn push(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<String, FerryError> {
    let destination = RemotePath::parse(remote_path).map_err(from_path)?;
    if destination.is_root() {
        return Err(from_path(PathError::Empty));
    }
    let source = store_local_path(local_path)?;
    let key = key_from_hex(device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;

    let id = {
        let mut state = lock(&shared.state);
        if !state.started {
            return Err(failed("Runtime::NotStarted"));
        }
        if state.peers.get(&key).is_none() {
            return Err(failed("Runtime::NotPaired"));
        }
        let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        let id = format!("{device_key_hex}-{session}");
        let file_name = engine::leaf_of(&destination);
        state.transfers.insert(
            id.clone(),
            TransferRow {
                id: id.clone(),
                device_key_hex: device_key_hex.to_owned(),
                file_name,
                source,
                destination: destination.clone(),
                bytes_total: 0,
                bytes_done: 0,
                state: TransferState::Queued,
                transport: None,
                error: None,
                source_size: None,
                source_mtime: None,
                running: false,
                attempt_after: None,
                backoff: BACKOFF_MIN,
                started_unix_secs: now_unix_secs(),
                ended_unix_secs: None,
                direction: Direction::Push,
                speed_bytes_per_sec: None,
                batch_id: None,
                chunk_size: *lock(&shared.chunk_size),
            },
        );
        id
    };

    // docs/engine-contract.md item 5: one Write entry per pushed file, with
    // the bytes, through the existing `record_this`. A best-effort local
    // size: if the path turns out to be missing, a directory, a symlink, or
    // a special file, the queued row will fail and say so, and no bytes
    // will really have moved, but the log entry itself is not worth
    // withholding over a size that could not be read.
    engine::record_this(
        shared,
        device_key_hex,
        access::AccessVerb::Write,
        destination.as_str(),
        Some(local_file_size(local_path)),
        None,
        None,
    );

    engine::notify(shared, Change::Transfers);
    transfer::spawn(shared, &id);
    Ok(id)
}

/// Send several files into one folder, as one batch. See `Engine::push_files`
/// in `engine.rs`.
pub(crate) fn push_files(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    local_paths: &[String],
    remote_folder: &str,
) -> Result<String, FerryError> {
    let folder = RemotePath::parse(remote_folder).map_err(from_path)?;
    let key = key_from_hex(device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
    {
        let state = lock(&shared.state);
        if !state.started {
            return Err(failed("Runtime::NotStarted"));
        }
        if state.peers.get(&key).is_none() {
            return Err(failed("Runtime::NotPaired"));
        }
    }

    // Confirm the folder is really one before anything is queued, the same
    // way `pull_folder` confirms its own folder by listing it. This dial
    // never wraps its stream in `Cut`; only a transfer attempt's dial does.
    let (stream, socket, addr, via) = transfer::dial(shared, device_key_hex, &key)?;
    engine::mark_reachable(shared, device_key_hex, addr, via);
    let connection_id = shared.next_connection_id();
    let _socket = engine::SocketRegistration::new(shared, connection_id, socket);
    let mut stream = StopAware::new(stream, Arc::clone(&shared.stopping));
    exchange_hello(&mut stream, &shared.display_name, shared.kind)
        .map_err(|error| from_rpc(&error))?;
    let mut client = Client::new(stream);
    let entry = client.stat(&folder).map_err(|error| from_rpc(&error))?;
    if entry.kind != FileKind::Directory {
        return Err(from_op(OpError::NotADirectory));
    }

    let started_unix_secs = now_unix_secs();
    let chunk_size = *lock(&shared.chunk_size);
    let mut rows = Vec::with_capacity(local_paths.len());
    let mut total_bytes: u64 = 0;
    for local_path in local_paths {
        let leaf = local_leaf_name(local_path);
        let destination =
            RemotePath::parse(&format!("{}/{leaf}", folder.as_str())).map_err(from_path)?;
        let source = store_local_path(local_path)?;
        total_bytes = total_bytes.saturating_add(local_file_size(local_path));
        let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        let id = format!("{device_key_hex}-{session}");
        rows.push(TransferRow {
            id,
            device_key_hex: device_key_hex.to_owned(),
            file_name: leaf,
            source,
            destination,
            bytes_total: 0,
            bytes_done: 0,
            state: TransferState::Queued,
            transport: None,
            error: None,
            source_size: None,
            source_mtime: None,
            running: false,
            attempt_after: None,
            backoff: BACKOFF_MIN,
            started_unix_secs,
            ended_unix_secs: None,
            direction: Direction::Push,
            speed_bytes_per_sec: None,
            // Filled in below, once the batch id exists.
            batch_id: None,
            chunk_size,
        });
    }

    // docs/engine-contract.md item 5: a file pushed as part of `push_files`
    // logs nothing of its own; the batch logs one Write entry for the
    // folder, the mirror of the folder copy rule in item 13.
    engine::record_this(
        shared,
        device_key_hex,
        access::AccessVerb::Write,
        folder.as_str(),
        Some(total_bytes),
        None,
        Some(u32::try_from(rows.len()).unwrap_or(u32::MAX)),
    );

    let batch_session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
    let batch_id = format!("{device_key_hex}-{batch_session}");
    for row in &mut rows {
        row.batch_id = Some(batch_id.clone());
    }
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let batch_row = crate::state::BatchRow {
        id: batch_id.clone(),
        device_key_hex: device_key_hex.to_owned(),
        label: remote_folder.to_owned(),
        direction: Direction::Push,
        origin: Origin::Manual,
        started_unix_secs,
        transfer_ids: ids.clone(),
        done_files: 0,
        done_bytes: 0,
    };
    batch::write_batch(&shared.batch_path(&batch_id), &BatchRecord::of(&batch_row))?;

    {
        let mut state = lock(&shared.state);
        state.batches.insert(batch_id.clone(), batch_row);
        for row in rows {
            state.transfers.insert(row.id.clone(), row);
        }
    }
    engine::notify(shared, Change::Transfers);
    for id in &ids {
        transfer::spawn(shared, id);
    }
    Ok(batch_id)
}

// ---------------------------------------------------------------------------
// Local paths, stored as a `RemotePath` with the leading slash stripped.
// ---------------------------------------------------------------------------

/// Turn an absolute local path into the `RemotePath` a `TransferRow` stores
/// it as. See the module documentation for why.
///
/// # Errors
///
/// Returns `OpError::NotFound` when `local_path` is not absolute, since
/// there is no root to make it relative to. Returns the matching
/// `PathError` code when the stripped path still fails `RemotePath::parse`,
/// such as a `..` component.
fn store_local_path(local_path: &str) -> Result<RemotePath, FerryError> {
    let stripped = local_path
        .strip_prefix('/')
        .ok_or_else(|| from_op(OpError::NotFound))?;
    RemotePath::parse(stripped).map_err(from_path)
}

/// Undo `store_local_path`: the absolute path on this device.
fn local_absolute_path(source: &RemotePath) -> String {
    format!("/{}", source.as_str())
}

/// A best-effort size for the access log entry `push` and `push_files` write
/// up front. Zero when the path cannot be read at all; the queued row will
/// say why once a worker attempts it.
fn local_file_size(local_path: &str) -> u64 {
    std::fs::metadata(local_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// The last component of a local path, for `file_name` and for the name a
/// pushed file takes inside `remote_folder`.
fn local_leaf_name(local_path: &str) -> String {
    Path::new(local_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(local_path)
        .to_owned()
}

/// Open the local file's parent folder as its own `LocalFs`, and name the
/// file within it. The same refusals apply as everywhere else: no symlink,
/// no special file, checked on the opened handle rather than the path,
/// exactly as `crate::guard::GuardedFs` already relies on `LocalFs` for on
/// the serving side.
fn open_local(source: &RemotePath) -> Result<(LocalFs, RemotePath), Outcome> {
    let absolute = local_absolute_path(source);
    let path = Path::new(&absolute);
    let (Some(parent), Some(leaf)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Err(Outcome::Fatal(from_op(OpError::NotFound)));
    };
    let fs = LocalFs::open(parent).map_err(|error| Outcome::Fatal(from_op(error)))?;
    let leaf_path = RemotePath::parse(leaf).map_err(|error| Outcome::Fatal(from_path(error)))?;
    Ok((fs, leaf_path))
}

// ---------------------------------------------------------------------------
// One attempt, called from `crate::transfer::attempt` once a connection and
// a `hello` exchange are already in hand.
// ---------------------------------------------------------------------------

/// One connection, one go at sending the file. Mirrors the shape of
/// `transfer::attempt`'s own pull half: read or build the record, then run
/// the transfer.
pub(crate) fn attempt<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    client: &mut Client<S>,
) -> Outcome {
    let transfer = match load_or_build(shared, id, plan) {
        Ok(transfer) => transfer,
        Err(outcome) => return outcome,
    };
    match send(shared, id, plan, client, &transfer) {
        Ok(()) => Outcome::Done,
        Err(outcome) => outcome,
    }
}

/// Read the stored record, or build one from the local file. See the module
/// documentation for why a push has no `FirstPass` stage of its own.
fn load_or_build(shared: &Arc<Shared>, id: &str, plan: &Plan) -> Result<Transfer, Outcome> {
    match read_record(&shared.record_path(id)) {
        Ok(Some(Record::Ready(_meta, transfer))) => Ok(transfer),
        Ok(Some(Record::FirstPass(..)) | None) => build(shared, id, plan),
        Err(error) => Err(Outcome::Fatal(error)),
    }
}

/// Open the local file, build its manifest, and store the record. Purely
/// local: nothing here needs the connection.
///
/// docs/engine-contract.md item 5: a local path that is missing, a
/// directory, a symlink, or a special file surfaces as the matching
/// `OpError` here.
fn build(shared: &Arc<Shared>, id: &str, plan: &Plan) -> Result<Transfer, Outcome> {
    let (fs, leaf) = open_local(&plan.source)?;
    let entry = fs
        .stat(&leaf)
        .map_err(|error| Outcome::Fatal(from_op(error)))?;
    if entry.kind != FileKind::File {
        return Err(Outcome::Fatal(from_op(OpError::IsADirectory)));
    }
    let manifest = fs
        .manifest(&leaf)
        .map_err(|error| Outcome::Fatal(from_op(error)))?;
    {
        let mut state = lock(&shared.state);
        if let Some(row) = state.transfers.get_mut(id) {
            row.bytes_total = manifest.length();
            row.source_size = Some(manifest.length());
            row.source_mtime = Some(entry.modified_unix_secs);
        }
    }
    let transfer = Transfer::new(manifest, plan.source.clone(), plan.destination.clone())
        .map_err(|error| Outcome::Fatal(from_transfer(&error)))?;
    let meta = Meta {
        started_unix_secs: plan.started_unix_secs,
        ended_unix_secs: None,
        direction: Direction::Push,
        batch_id: plan.batch_id.clone(),
    };
    write_record(
        &shared.record_path(id),
        &Record::Ready(meta, transfer.clone()),
    )
    .map_err(Outcome::Fatal)?;
    Ok(transfer)
}

/// Send every chunk the peer's partial does not already hold, then land it.
fn send<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    client: &mut Client<S>,
    transfer: &Transfer,
) -> Result<(), Outcome> {
    let manifest = &transfer.manifest;
    let (fs, leaf) = open_local(&plan.source)?;
    let partial = partial_path(&plan.destination).map_err(Outcome::Fatal)?;

    {
        let mut state = lock(&shared.state);
        if let Some(row) = state.transfers.get_mut(id) {
            row.bytes_total = manifest.length();
        }
    }

    // A `rename` or `set_mtime` whose request reached the peer but whose
    // response was lost still succeeded there: the partial is gone because
    // it already became the real file. Ask the real path's own manifest
    // before assuming nothing has arrived, and set the modified time again
    // either way, since that call is idempotent and may be the one whose
    // response was the one that was lost.
    if landed_manifest(client, &plan.destination)?.is_some_and(|remote| remote == *manifest) {
        let mtime = fs.stat(&leaf).map_or(0, |entry| entry.modified_unix_secs);
        client
            .set_mtime(&plan.destination, mtime)
            .map_err(|error| classify_rpc(&error))?;
        return Ok(());
    }

    let mut reporter = Reporter::new(shared, id, &plan.device_key_hex);
    let start = resume_point(client, &partial, manifest)?;

    if manifest.length() == 0 {
        // An empty file still needs its partial to exist, so the rename
        // below has something to rename.
        client
            .write(&partial, 0, Vec::new())
            .map_err(|error| classify_rpc(&error))?;
    }

    for index in start..manifest.chunk_count() {
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = fs
            .read(&leaf, offset, length)
            .map_err(|error| Outcome::Fatal(from_op(error)))?;
        if bytes.len() != usize::try_from(length).unwrap_or(usize::MAX) {
            // The local file no longer holds what its own manifest said it
            // did.
            return Err(Outcome::Fatal(failed("TransferError::ShortRead")));
        }
        write_all_remote(client, &partial, offset, &bytes).map_err(|error| classify_rpc(&error))?;
        reporter.moved(offset + u64::from(length), manifest.length());
        if shared.stopping() {
            return Err(Outcome::Retry(failed("Runtime::NotReachable")));
        }
    }

    // A partial left over from an earlier, longer version of the local file
    // may hold more than this manifest needs.
    client
        .truncate(&partial, manifest.length())
        .map_err(|error| classify_rpc(&error))?;

    // Ask once more. A matching manifest means the peer now holds the whole
    // file; anything else is worth another attempt.
    match client.manifest(&partial) {
        Ok(remote) if remote == *manifest => {
            client
                .rename(&partial, &plan.destination)
                .map_err(|error| classify_rpc(&error))?;
            let mtime = fs.stat(&leaf).map_or(0, |entry| entry.modified_unix_secs);
            client
                .set_mtime(&plan.destination, mtime)
                .map_err(|error| classify_rpc(&error))?;
            Ok(())
        }
        Ok(remote) => Err(Outcome::Retry(from_transfer(
            &TransferError::ChunkFailedVerification {
                index: first_mismatch(manifest, &remote),
            },
        ))),
        Err(RpcError::Remote(OpError::NotFound)) => Err(Outcome::Retry(from_transfer(
            &TransferError::ChunkFailedVerification { index: 0 },
        ))),
        Err(error) => Err(classify_rpc(&error)),
    }
}

/// The peer's manifest for the file already at its real name, or `None` when
/// nothing is there yet. Used only to notice a `rename` that landed on an
/// earlier attempt whose response never arrived.
fn landed_manifest<S: Read + Write>(
    client: &mut Client<S>,
    destination: &RemotePath,
) -> Result<Option<Manifest>, Outcome> {
    match client.manifest(destination) {
        Ok(remote) => Ok(Some(remote)),
        Err(RpcError::Remote(OpError::NotFound)) => Ok(None),
        Err(error) => Err(classify_rpc(&error)),
    }
}

/// Ask the peer for the manifest of the partial file, and find the first
/// chunk that differs from `manifest`. `Ok(0)` when the partial does not
/// exist yet: nothing has arrived, so everything is new.
///
/// docs/protocol.md section 9, "the manifest is a hint, the disk is the
/// truth": a pusher has no local disk to re-hash for this, so it asks the
/// peer instead, fresh on every attempt, never trusting a stored value.
fn resume_point<S: Read + Write>(
    client: &mut Client<S>,
    partial: &RemotePath,
    manifest: &Manifest,
) -> Result<usize, Outcome> {
    match client.manifest(partial) {
        Ok(remote) => Ok(first_mismatch(manifest, &remote)),
        Err(RpcError::Remote(OpError::NotFound)) => Ok(0),
        Err(error) => Err(classify_rpc(&error)),
    }
}

/// The first index where `remote`'s chaining value disagrees with `local`'s,
/// or the number of chunks they have in common when every one of those
/// matches. No bytes are re-read on either side to answer this: comparing
/// the two manifests' chaining values directly is the whole check.
fn first_mismatch(local: &Manifest, remote: &Manifest) -> usize {
    let common = local.chunk_count().min(remote.chunk_count());
    (0..common)
        .find(|&index| local.chunks()[index] != remote.chunks()[index])
        .unwrap_or(common)
}

/// Where a push writes until the whole file verifies on the peer.
/// `docs/engine-contract.md` item 5: a fixed suffix, not a per-attempt
/// random one, so a fresh attempt on the same transfer continues the same
/// partial rather than starting a new one.
fn partial_path(destination: &RemotePath) -> Result<RemotePath, FerryError> {
    RemotePath::parse(&format!("{}.ferry-part", destination.as_str())).map_err(from_path)
}

/// Write one range to the peer, in pieces one `write` call accepts.
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
