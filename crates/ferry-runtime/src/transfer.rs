//! One transfer, on one thread, until it is done or it fails.
//!
//! # Why the first pass builds the manifest
//!
//! `session::pull` needs a manifest before it starts, and only the device
//! that holds the file can compute one. Version 1 of the file operations
//! layer has no operation that asks for a manifest, and this crate may not
//! add one.
//!
//! So the first pass does both jobs at once. It reads the whole file in
//! chunk sized pieces, feeds each piece to a `ManifestBuilder`, and writes
//! the same piece to the partial file. That is one pass over the network,
//! not two. The manifest is then written to disk beside the partial file,
//! and every later attempt is an ordinary `session::pull` that resumes from
//! the persisted manifest.
//!
//! # What that costs
//!
//! On the first pass the manifest describes what arrived, so it cannot catch
//! a device that sends the wrong bytes. From the moment the manifest is on
//! disk it can, and it does: a file that changes on the phone between the
//! first pass and a resume makes chunk verification fail, and the transfer
//! stops. That is the designed behaviour, per `docs/protocol.md` section 9.
//!
//! A first pass that is interrupted has no manifest yet, so it starts again
//! from the first byte the partial file does not already hold. The size and
//! the modified time of the source are compared on every attempt; a source
//! that changed makes the pass start from zero.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_core::chunk::{ChunkSize, ManifestBuilder};
use ferry_core::limits;
use ferry_core::noise::PublicKey;
use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, RpcError, exchange_hello};
use ferry_core::session::{Progress, Transfer, TransferError, pull_with_progress};
use ferry_core::tcp;

use crate::engine::{Shared, dial_targets, mark_reachable};
use crate::errors::{failed, from_op, from_rpc, from_transfer};
use crate::guard::StopAware;
use crate::state::{key_from_hex, lock};
use crate::{FerryError, TransferState, Transport};

/// The shortest wait before trying again.
const BACKOFF_MIN: Duration = Duration::from_secs(1);

/// The longest wait before trying again.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How often the app is told a transfer moved.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// How one attempt ended.
enum Outcome {
    /// The file is at its final name.
    Done,
    /// The link failed. Wait, then try again.
    Retry(FerryError),
    /// Trying again would fail the same way.
    Fatal(FerryError),
}

/// Start a thread for every transfer that is not finished.
pub(crate) fn resume_all(shared: &Arc<Shared>) {
    let ids: Vec<String> = lock(&shared.state)
        .transfers
        .values()
        .filter(|row| !matches!(row.state, TransferState::Done | TransferState::Failed))
        .map(|row| row.id.clone())
        .collect();
    for id in ids {
        spawn(shared, &id);
    }
}

/// Start the thread that carries one transfer.
pub(crate) fn spawn(shared: &Arc<Shared>, id: &str) {
    {
        let mut state = lock(&shared.state);
        let Some(row) = state.transfers.get_mut(id) else {
            return;
        };
        if row.running {
            return;
        }
        row.running = true;
    }
    let shared_for_thread = Arc::clone(shared);
    let id = id.to_owned();
    shared.keep(std::thread::spawn(move || {
        run(&shared_for_thread, &id);
        lock(&shared_for_thread.state)
            .transfers
            .entry(id)
            .and_modify(|row| row.running = false);
    }));
}

/// Try, wait, try again, until the transfer is done or cannot continue.
fn run(shared: &Arc<Shared>, id: &str) {
    let mut backoff = BACKOFF_MIN;
    loop {
        if shared.stopping() {
            return;
        }
        match attempt(shared, id) {
            Outcome::Done => {
                finish(shared, id, TransferState::Done, None);
                // The record's only job was to survive a restart, and there
                // is nothing left to survive.
                drop(std::fs::remove_file(shared.record_path(id)));
                return;
            }
            Outcome::Fatal(error) => {
                finish(shared, id, TransferState::Failed, Some(error));
                return;
            }
            Outcome::Retry(error) => {
                finish(shared, id, TransferState::Paused, Some(error));
                if !shared.rest(backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Write a transfer's new state down and tell the app.
fn finish(shared: &Arc<Shared>, id: &str, state: TransferState, error: Option<FerryError>) {
    {
        let mut locked = lock(&shared.state);
        let Some(row) = locked.transfers.get_mut(id) else {
            return;
        };
        row.state = state;
        row.error = error;
        if state != TransferState::Active {
            row.transport = None;
        }
        if state == TransferState::Done {
            row.bytes_done = row.bytes_total;
        }
    }
    shared.listener.transfers_changed();
    shared.listener.devices_changed();
}

/// What one attempt needs to know, copied out from under the lock.
struct Plan {
    device_key_hex: String,
    peer: PublicKey,
    source: RemotePath,
    destination: RemotePath,
    source_size: Option<u64>,
    source_mtime: Option<i64>,
    /// The 32 hex characters that name this transfer's own partial file
    /// during the first pass.
    suffix: String,
}

/// Read the plan for one transfer out of the state.
fn plan_for(shared: &Arc<Shared>, id: &str) -> Option<Plan> {
    let state = lock(&shared.state);
    let row = state.transfers.get(id)?;
    let peer = key_from_hex(&row.device_key_hex)?;
    Some(Plan {
        device_key_hex: row.device_key_hex.clone(),
        peer,
        source: row.source.clone(),
        destination: row.destination.clone(),
        source_size: row.source_size,
        source_mtime: row.source_mtime,
        suffix: id.rsplit('-').next().unwrap_or(id).to_owned(),
    })
}

/// One connection, one go at moving the file.
fn attempt(shared: &Arc<Shared>, id: &str) -> Outcome {
    let Some(plan) = plan_for(shared, id) else {
        return Outcome::Fatal(failed("Runtime::TransferNotFound"));
    };
    let Some(fs) = shared.shared_fs() else {
        return Outcome::Fatal(failed("Runtime::NotStarted"));
    };

    let (stream, addr, via) = match dial(shared, &plan) {
        Ok(found) => found,
        Err(error) => return Outcome::Retry(error),
    };
    mark_reachable(shared, &plan.device_key_hex, addr, via);

    // The wrapper fails the next read once `stop` runs, so a transfer does
    // not hold `stop` for a whole file.
    let mut stream = StopAware::new(stream, Arc::clone(&shared.stopping));
    if let Err(error) = exchange_hello(&mut stream, &shared.display_name) {
        return Outcome::Retry(from_rpc(&error));
    }
    {
        let mut state = lock(&shared.state);
        if let Some(row) = state.transfers.get_mut(id) {
            row.state = TransferState::Active;
            row.transport = Some(via);
            row.error = None;
        }
    }
    shared.listener.transfers_changed();

    let mut client = Client::new(stream);
    let record = match load_or_build(shared, id, &plan, fs.as_ref(), &mut client) {
        Ok(record) => record,
        Err(outcome) => return outcome,
    };
    verify_and_land(shared, id, &plan, fs.as_ref(), &mut client, &record)
}

/// Find a way to reach the device, best path first.
fn dial(
    shared: &Arc<Shared>,
    plan: &Plan,
) -> Result<(ferry_core::noise::SecureStream, SocketAddr, Transport), FerryError> {
    for (addr, via) in dial_targets(shared, &plan.device_key_hex) {
        if shared.stopping() {
            break;
        }
        if let Ok(connection) = tcp::connect(addr, &shared.key, &plan.peer) {
            return Ok((connection.stream, connection.remote, via));
        }
    }
    Err(failed("Runtime::NotReachable"))
}

/// Read the stored record, or run the first pass and write one.
fn load_or_build<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    fs: &dyn FileOps,
    client: &mut Client<S>,
) -> Result<Transfer, Outcome> {
    if let Ok(bytes) = std::fs::read(shared.record_path(id)) {
        return match Transfer::decode(&bytes) {
            Ok(record) => Ok(record),
            Err(error) => Err(Outcome::Fatal(from_transfer(&error))),
        };
    }
    first_pass(shared, id, plan, fs, client)
}

/// Read the whole file once, building the manifest as the bytes arrive.
fn first_pass<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    fs: &dyn FileOps,
    client: &mut Client<S>,
) -> Result<Transfer, Outcome> {
    let entry = match client.stat(&plan.source) {
        Ok(entry) => entry,
        Err(error) => return Err(classify_rpc(&error)),
    };
    if entry.kind != FileKind::File {
        return Err(Outcome::Fatal(from_op(OpError::IsADirectory)));
    }
    let size = entry.size;
    let changed = plan.source_size.is_some_and(|old| old != size)
        || plan
            .source_mtime
            .is_some_and(|old| old != entry.modified_unix_secs);
    {
        let mut state = lock(&shared.state);
        if let Some(row) = state.transfers.get_mut(id) {
            row.bytes_total = size;
            row.source_size = Some(size);
            row.source_mtime = Some(entry.modified_unix_secs);
        }
    }

    let temporary = temp_path(&plan.destination, &plan.suffix).map_err(Outcome::Fatal)?;
    ensure_parents(fs, &plan.destination).map_err(|e| Outcome::Fatal(from_op(e)))?;
    let chunk = ChunkSize::one_mebibyte();
    let start =
        start_offset(fs, &temporary, chunk, changed).map_err(|e| Outcome::Fatal(from_op(e)))?;

    let mut builder = ManifestBuilder::new(chunk);
    rehash_local(fs, &temporary, chunk, start, &mut builder)
        .map_err(|e| Outcome::Fatal(from_op(e)))?;
    let span = Span {
        chunk,
        start,
        size,
        temporary: temporary.clone(),
    };
    fetch_rest(shared, id, plan, fs, client, &span, &mut builder)?;

    let record = Transfer::new(
        builder.finish(),
        plan.source.clone(),
        plan.destination.clone(),
    )
    .map_err(|e| Outcome::Fatal(from_transfer(&e)))?;
    let landing = record
        .temporary_path()
        .map_err(|e| Outcome::Fatal(from_transfer(&e)))?;
    fs.rename(&temporary, &landing)
        .map_err(|e| Outcome::Fatal(from_op(e)))?;
    std::fs::write(shared.record_path(id), record.encode())
        .map_err(|_| Outcome::Fatal(failed("TransferError::Local")))?;
    Ok(record)
}

/// Where the first pass writes, and how far it has to go.
struct Span {
    /// The chunk size the manifest is built with.
    chunk: ChunkSize,
    /// The first byte still to fetch.
    start: u64,
    /// How long the source file is.
    size: u64,
    /// The partial file this pass writes to.
    temporary: RemotePath,
}

/// Fetch every chunk the partial file does not already hold.
fn fetch_rest<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    fs: &dyn FileOps,
    client: &mut Client<S>,
    span: &Span,
    builder: &mut ManifestBuilder,
) -> Result<(), Outcome> {
    let mut reporter = Reporter::new(shared, id, &plan.device_key_hex);
    let mut offset = span.start;
    if span.size == 0 {
        // An empty file still needs its partial file to exist, because the
        // pull that follows truncates and renames it.
        fs.write(&span.temporary, 0, &[])
            .map_err(|e| Outcome::Fatal(from_op(e)))?;
    }
    while offset < span.size {
        let want = u32::try_from((span.size - offset).min(span.chunk.as_u64()))
            .unwrap_or(span.chunk.get());
        let bytes = match fetch_remote(client, &plan.source, offset, want) {
            Ok(bytes) => bytes,
            Err(error) => return Err(classify_rpc(&error)),
        };
        if bytes.len() != usize::try_from(want).unwrap_or(usize::MAX) {
            // The device holds less of the file than it said it holds.
            return Err(Outcome::Fatal(failed("TransferError::ShortRead")));
        }
        write_all_local(fs, &span.temporary, offset, &bytes)
            .map_err(|e| Outcome::Fatal(from_op(e)))?;
        builder.push(&bytes);
        offset += u64::from(want);
        reporter.moved(offset, span.size);
        if shared.stopping() {
            return Err(Outcome::Retry(failed("Runtime::NotReachable")));
        }
    }
    Ok(())
}

/// Run the pull, which verifies every chunk, then renames the file.
fn verify_and_land<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    fs: &dyn FileOps,
    client: &mut Client<S>,
    record: &Transfer,
) -> Outcome {
    {
        let mut state = lock(&shared.state);
        if let Some(row) = state.transfers.get_mut(id) {
            row.bytes_total = record.manifest.length();
        }
    }
    let mut reporter = Reporter::new(shared, id, &plan.device_key_hex);
    let total = record.manifest.length();
    let result = pull_with_progress(client, record, fs, |progress: Progress| {
        reporter.moved(progress.bytes_done, total);
    });
    match result {
        Ok(_) => Outcome::Done,
        Err(error) => classify_transfer(&error),
    }
}

/// A connection failure is worth another go. A refusal by the peer is not.
fn classify_rpc(error: &RpcError) -> Outcome {
    match error {
        RpcError::Remote(inner) => Outcome::Fatal(from_op(*inner)),
        other => Outcome::Retry(from_rpc(other)),
    }
}

/// Which transfer failures are worth another go.
fn classify_transfer(error: &TransferError) -> Outcome {
    match error {
        // A refusal by the peer will be refused again.
        TransferError::Rpc(RpcError::Remote(inner)) => Outcome::Fatal(from_op(*inner)),
        TransferError::Rpc(_) => Outcome::Retry(from_transfer(error)),
        TransferError::ChunkFailedVerification { .. }
        | TransferError::ShortRead { .. }
        | TransferError::Local(_)
        | TransferError::Record(_)
        | TransferError::BadPath(_)
        | TransferError::NoRandomness => Outcome::Fatal(from_transfer(error)),
    }
}

/// Where the first pass writes, before the manifest exists.
fn temp_path(destination: &RemotePath, suffix: &str) -> Result<RemotePath, FerryError> {
    RemotePath::parse(&format!("{}.{suffix}.part", destination.as_str()))
        .map_err(crate::errors::from_path)
}

/// How many bytes of the partial file are whole chunks worth keeping.
///
/// A link that died mid chunk leaves a part of one behind. Only whole chunks
/// are kept, because `ManifestBuilder` needs every chunk but the last to be
/// exactly one chunk long.
fn start_offset(
    fs: &dyn FileOps,
    temporary: &RemotePath,
    chunk: ChunkSize,
    changed: bool,
) -> Result<u64, OpError> {
    let held = match fs.stat(temporary) {
        Ok(entry) => entry.size,
        Err(OpError::NotFound) => return Ok(0),
        Err(other) => return Err(other),
    };
    let keep = if changed {
        0
    } else {
        (held / chunk.as_u64()) * chunk.as_u64()
    };
    if keep != held {
        fs.truncate(temporary, keep)?;
    }
    Ok(keep)
}

/// Hash the chunks the partial file already holds, so the builder can carry
/// on from there.
fn rehash_local(
    fs: &dyn FileOps,
    temporary: &RemotePath,
    chunk: ChunkSize,
    upto: u64,
    builder: &mut ManifestBuilder,
) -> Result<(), OpError> {
    let mut offset = 0u64;
    while offset < upto {
        let want = u32::try_from((upto - offset).min(chunk.as_u64())).unwrap_or(chunk.get());
        let bytes = read_all_local(fs, temporary, offset, want)?;
        if bytes.len() != usize::try_from(want).unwrap_or(usize::MAX) {
            // The file shrank under us. Everything after this point is gone.
            return Ok(());
        }
        builder.push(&bytes);
        offset += u64::from(want);
    }
    Ok(())
}

/// Read one range from the peer, in pieces one message can carry.
fn fetch_remote<S: Read + Write>(
    client: &mut Client<S>,
    path: &RemotePath,
    offset: u64,
    want: u32,
) -> Result<Vec<u8>, RpcError> {
    let mut out: Vec<u8> = Vec::new();
    while let Some(piece) = next_piece(out.len(), want) {
        let at = offset + u64::try_from(out.len()).unwrap_or(0);
        let got = client.read(path, at, piece)?;
        if got.is_empty() {
            break;
        }
        out.extend_from_slice(&got);
    }
    Ok(out)
}

/// Read one range from local storage, in pieces one call can carry.
fn read_all_local(
    fs: &dyn FileOps,
    path: &RemotePath,
    offset: u64,
    want: u32,
) -> Result<Vec<u8>, OpError> {
    let mut out: Vec<u8> = Vec::new();
    while let Some(piece) = next_piece(out.len(), want) {
        let at = offset + u64::try_from(out.len()).unwrap_or(0);
        let got = fs.read(path, at, piece)?;
        if got.is_empty() {
            break;
        }
        out.extend_from_slice(&got);
    }
    Ok(out)
}

/// How many bytes to ask for next, or `None` when the range is complete.
fn next_piece(done: usize, want: u32) -> Option<u32> {
    let done = u32::try_from(done).unwrap_or(want);
    if done >= want {
        return None;
    }
    Some((want - done).min(limits::MAX_READ_LEN))
}

/// Write one range to local storage, in pieces one call can carry.
fn write_all_local(
    fs: &dyn FileOps,
    path: &RemotePath,
    offset: u64,
    bytes: &[u8],
) -> Result<(), OpError> {
    let cap = usize::try_from(limits::MAX_WRITE_LEN).unwrap_or(usize::MAX);
    let mut written = 0usize;
    while written < bytes.len() {
        let piece = (bytes.len() - written).min(cap);
        let at = offset + u64::try_from(written).unwrap_or(0);
        let n = fs.write(path, at, &bytes[written..written + piece])?;
        if n == 0 {
            return Err(OpError::Internal);
        }
        written += usize::try_from(n).unwrap_or(piece);
    }
    Ok(())
}

/// Make every folder above the destination, so a write has somewhere to go.
fn ensure_parents(fs: &dyn FileOps, destination: &RemotePath) -> Result<(), OpError> {
    let parts: Vec<&str> = destination.components().collect();
    let mut so_far = String::new();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if !so_far.is_empty() {
            so_far.push('/');
        }
        so_far.push_str(part);
        let path = RemotePath::parse(&so_far).map_err(|_| OpError::InvalidPath)?;
        match fs.mkdir(&path) {
            Ok(()) | Err(OpError::AlreadyExists) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(())
}

/// Writes progress down, and tells the app at most once a second.
struct Reporter<'a> {
    shared: &'a Arc<Shared>,
    id: String,
    device_key_hex: String,
    last_report: Instant,
    bytes_at_last_report: u64,
}

impl<'a> Reporter<'a> {
    /// Start reporting for one transfer.
    fn new(shared: &'a Arc<Shared>, id: &str, device_key_hex: &str) -> Self {
        Self {
            shared,
            id: id.to_owned(),
            device_key_hex: device_key_hex.to_owned(),
            last_report: Instant::now(),
            bytes_at_last_report: 0,
        }
    }

    /// Note that the transfer has reached `bytes_done` of `total`.
    fn moved(&mut self, bytes_done: u64, total: u64) {
        let elapsed = self.last_report.elapsed();
        let due = elapsed >= REPORT_EVERY;
        let speed = if due {
            let moved = bytes_done.saturating_sub(self.bytes_at_last_report);
            // Whole seconds only. Reporting happens once a second at most,
            // so a fraction of a second never divides this.
            Some(moved / elapsed.as_secs().max(1))
        } else {
            None
        };
        {
            let mut state = lock(&self.shared.state);
            if let Some(row) = state.transfers.get_mut(&self.id) {
                row.bytes_done = bytes_done;
                row.bytes_total = total;
            }
            if due {
                state.live_mut(&self.device_key_hex).speed_bytes_per_sec = speed;
            }
        }
        if due {
            self.last_report = Instant::now();
            self.bytes_at_last_report = bytes_done;
            self.shared.listener.transfers_changed();
            self.shared.listener.devices_changed();
        }
    }
}

impl Drop for Reporter<'_> {
    fn drop(&mut self) {
        // A transfer that stopped is no longer moving bytes, so the speed
        // shown beside its device has to go.
        lock(&self.shared.state)
            .live
            .entry(self.device_key_hex.clone())
            .or_default()
            .speed_bytes_per_sec = None;
    }
}
