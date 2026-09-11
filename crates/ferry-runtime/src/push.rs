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

use ferry_core::chunk::{ChunkSize, Manifest};
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

    // H3: the Write entry is logged from the bytes actually written, at the
    // end of a successful attempt (`record_attempt_write`), the mirror of
    // a pull's `record_attempt_read`. Logging it here instead, up front,
    // would count bytes before the peer ever verified them, and would
    // still count them even when the push fails and nothing moves at all.
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
    if folder.is_root() {
        return Err(from_path(PathError::Empty));
    }
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
    let mut rows = rows_for_files(
        local_paths,
        device_key_hex,
        &folder,
        started_unix_secs,
        chunk_size,
    )?;

    // H3: a file pushed as part of `push_files` logs nothing of its own;
    // the batch logs one Write entry for the folder, from the copied count
    // and bytes once every file in it has reached `Done` or `Failed`
    // (`transfer::finish`, which calls `record_batch_write` below). Logging
    // it here instead, up front, would count files and bytes before any of
    // them had actually landed.

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

/// Build one `TransferRow`, still without a batch id, for each file
/// `push_files` was given. Mirrors `rows_for_folder` in `engine.rs`: every
/// row is built before anything touches state, so a failure part way
/// through leaves nothing behind.
fn rows_for_files(
    local_paths: &[String],
    device_key_hex: &str,
    folder: &RemotePath,
    started_unix_secs: i64,
    chunk_size: ChunkSize,
) -> Result<Vec<TransferRow>, FerryError> {
    let mut rows = Vec::with_capacity(local_paths.len());
    for local_path in local_paths {
        let leaf = local_leaf_name(local_path);
        let destination =
            RemotePath::parse(&format!("{}/{leaf}", folder.as_str())).map_err(from_path)?;
        let source = store_local_path(local_path)?;
        let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        rows.push(TransferRow {
            id: format!("{device_key_hex}-{session}"),
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
            // Filled in by the caller, once the batch id exists.
            batch_id: None,
            chunk_size,
        });
    }
    Ok(rows)
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
///
/// H3: `connection` and `bytes_before` are exactly what `transfer::attempt`
/// passes its own `record_attempt_read` after a pull; this attempt's own
/// `record_attempt_write`, below, is the write-side mirror of that.
pub(crate) fn attempt<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    client: &mut Client<S>,
    connection: u64,
    bytes_before: u64,
) -> Outcome {
    let transfer = match load_or_build(shared, id, plan) {
        Ok(transfer) => transfer,
        Err(outcome) => {
            record_attempt_write(shared, id, plan, connection, bytes_before);
            return outcome;
        }
    };
    let outcome = match send(shared, id, plan, client, &transfer) {
        Ok(()) => Outcome::Done,
        Err(outcome) => outcome,
    };
    record_attempt_write(shared, id, plan, connection, bytes_before);
    outcome
}

/// Record what this attempt actually sent, as actor `This`, and end the
/// roll-up's connection for it. The write-side mirror of
/// `transfer::record_attempt_read`.
///
/// Skips logging when nothing was sent: an attempt that failed before a
/// byte crossed the wire has nothing to report. Skips logging when
/// `plan.batch_id` is `Some`: a file pushed as part of `push_files` logs
/// nothing of its own, because the batch's own entry, with its `files` and
/// `bytes` totals, already covers it once the batch ends
/// (`record_batch_write`, below).
fn record_attempt_write(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    connection: u64,
    bytes_before: u64,
) {
    if plan.batch_id.is_some() {
        return;
    }
    let sent = transfer::bytes_done_of(shared, id).saturating_sub(bytes_before);
    if sent == 0 {
        return;
    }
    let now = now_unix_secs();
    let mut log = lock(&shared.access_log);
    let Some(rollup) = log.as_mut() else {
        return;
    };
    rollup.touch(
        now,
        connection,
        access::EntryFields {
            device_key_hex: plan.device_key_hex.clone(),
            actor: access::Actor::This,
            verb: access::AccessVerb::Write,
            path: plan.destination.as_str().to_owned(),
            bytes: Some(sent),
            entries: None,
            files: None,
        },
    );
    rollup.connection_ended(now, connection);
}

/// Record one `push_files` batch's own Write entry, once every file in it
/// has reached `Done` or `Failed`: one entry for the folder, with the
/// copied count and bytes. Called from `transfer::finish`.
///
/// Best effort, the same as every other access log write: a dropped entry
/// here is a gap in the log, never a wrong answer, and never something a
/// transfer's own outcome depends on.
pub(crate) fn record_batch_write(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    label: &str,
    files_done: u32,
    bytes_done: u64,
) {
    engine::record_this(
        shared,
        device_key_hex,
        access::AccessVerb::Write,
        label,
        Some(bytes_done),
        None,
        Some(files_done),
    );
}

/// Read the stored record, or build one from the local file. See the module
/// documentation for why a push has no `FirstPass` stage of its own.
///
/// H2: a stored record is only trusted while the local file still matches
/// the size and modified time it had when that record's manifest was
/// built. A person can edit the file between two attempts of the same
/// push; a manifest built from what it used to be would describe bytes
/// that are no longer there, so it is rebuilt from what is on disk now
/// instead, the same as a push's very first attempt.
fn load_or_build(shared: &Arc<Shared>, id: &str, plan: &Plan) -> Result<Transfer, Outcome> {
    match read_record(&shared.record_path(id)) {
        Ok(Some(Record::Ready(_meta, transfer))) => {
            if local_file_changed(shared, id, &plan.source)? {
                build(shared, id, plan)
            } else {
                Ok(transfer)
            }
        }
        Ok(Some(Record::FirstPass(..)) | None) => build(shared, id, plan),
        Err(error) => Err(Outcome::Fatal(error)),
    }
}

/// Whether the local file at `source` no longer matches the size and
/// modified time this row's own `build` last recorded for it.
fn local_file_changed(
    shared: &Arc<Shared>,
    id: &str,
    source: &RemotePath,
) -> Result<bool, Outcome> {
    let (fs, leaf) = open_local(source)?;
    let entry = fs
        .stat(&leaf)
        .map_err(|error| Outcome::Fatal(from_op(error)))?;
    let (stored_size, stored_mtime) = {
        let state = lock(&shared.state);
        let row = state.transfers.get(id);
        (
            row.and_then(|row| row.source_size),
            row.and_then(|row| row.source_mtime),
        )
    };
    Ok(stored_size != Some(entry.size) || stored_mtime != Some(entry.modified_unix_secs))
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
    // file; anything else is worth another attempt, unless this attempt
    // already sent every chunk from offset 0: a rewrite that started from a
    // resume point may retry once from 0, but a rewrite that already
    // started from 0 and still does not verify will never verify, however
    // many more times it is tried. H1: this is what stops a peer that
    // acknowledges every write and stores nothing from retrying forever.
    let sent_from_zero = start == 0;
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
        Ok(remote) => {
            let error = TransferError::ChunkFailedVerification {
                index: first_mismatch(manifest, &remote),
            };
            Err(if sent_from_zero {
                Outcome::Fatal(from_transfer(&error))
            } else {
                Outcome::Retry(from_transfer(&error))
            })
        }
        Err(RpcError::Remote(OpError::NotFound)) => {
            let error = TransferError::ChunkFailedVerification { index: 0 };
            Err(if sent_from_zero {
                Outcome::Fatal(from_transfer(&error))
            } else {
                Outcome::Retry(from_transfer(&error))
            })
        }
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
        // H4: zero is a peer that wrote nothing; more than `piece` is a
        // peer claiming to have written bytes this call never sent it.
        // Trusting that claim moves `written` ahead by more than what was
        // actually sent, so the next piece is read from the wrong offset
        // in `bytes` and written to the wrong offset on the peer: bytes in
        // between are silently skipped, on both sides, rather than ever
        // erroring, until the final manifest check catches the mismatch as
        // a plain `ChunkFailedVerification`, far from where it happened.
        // Neither shape is one this side asked for, and the same peer
        // would claim it again on a retry, so this is fatal here instead,
        // not worth another attempt.
        if sent == 0 || usize::try_from(sent).unwrap_or(usize::MAX) > piece {
            return Err(RpcError::Remote(OpError::Internal));
        }
        written += usize::try_from(sent).unwrap_or(piece);
    }
    Ok(())
}
