//! Transfers, on a small pool of threads, until each is done or fails.
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
//! A first pass writes a record of its own at every chunk boundary, holding
//! the size of the source and how many whole chunks have arrived. So a first
//! pass that is cut short is picked up where it stopped, even after the app
//! has closed and opened again. Without that record the partial file was
//! left in the person's folder and the whole file was fetched again.
//!
//! The size and the modified time of the source are compared on every
//! attempt. A source that changed makes the pass start from zero.
//!
//! # Why there is a pool
//!
//! One thread per transfer meant that two hundred pulls made two hundred
//! threads, all dialing at once. A pool of four keeps the link busy, because
//! a transfer waits on the network and not on this machine. A transfer that
//! is waiting out a backoff gives its worker back and sits in the queue with
//! a time on it, so a queue of paused transfers never holds a worker asleep.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
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

use crate::engine::{
    Shared, dial_targets, mark_reachable, notify, remove_record, this_devices_kind,
};
use crate::errors::{failed, from_op, from_rpc, from_transfer};
use crate::guard::{Cut, StopAware};
use crate::notify::Change;
use crate::record::{FirstPass, Record, read_record, write_record};
use crate::state::{key_from_hex, lock};
use crate::{FerryError, TransferState, Transport};

/// The shortest wait before trying again.
pub(crate) const BACKOFF_MIN: Duration = Duration::from_secs(1);

/// The longest wait before trying again.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How often the app is told a transfer moved.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// How many transfers may move at once.
const MAX_WORKERS: usize = 4;

/// How long one chunk may take before the peer counts as stalled.
const CHUNK_DEADLINE: Duration = Duration::from_secs(30);

/// How one attempt ended.
enum Outcome {
    /// The file is at its final name.
    Done,
    /// The link failed. Wait, then try again.
    Retry(FerryError),
    /// Trying again would fail the same way.
    Fatal(FerryError),
}

/// What a worker found at the front of the queue.
enum Job {
    /// This transfer may be attempted now.
    Ready(String),
    /// Nothing may be attempted for this long.
    Wait(Duration),
}

/// Queue every transfer that is not finished, and drop the strays.
///
/// A record whose device is not paired any more is deleted rather than
/// resumed. Such a record is what brought a forgotten device back: the
/// transfer would dial it, and a name exchange would put it in the list
/// again.
pub(crate) fn resume_all(shared: &Arc<Shared>) {
    let (ids, strays) = {
        let mut state = lock(&shared.state);
        let mut ids: Vec<String> = Vec::new();
        let mut strays: Vec<String> = Vec::new();
        for row in state.transfers.values() {
            let paired = key_from_hex(&row.device_key_hex)
                .is_some_and(|key| state.peers.get(&key).is_some());
            if paired {
                if !matches!(row.state, TransferState::Done | TransferState::Failed) {
                    ids.push(row.id.clone());
                }
            } else {
                strays.push(row.id.clone());
            }
        }
        for id in &strays {
            state.transfers.remove(id);
        }
        (ids, strays)
    };
    for id in &strays {
        // A record that will not go is left alone. The row is gone either
        // way, so nothing dials that device in this run.
        drop(remove_record(shared, id));
    }
    if !strays.is_empty() {
        notify(shared, Change::Transfers);
    }
    for id in ids {
        spawn(shared, &id);
    }
}

/// Put one transfer in the queue, and start a worker if one is free.
pub(crate) fn spawn(shared: &Arc<Shared>, id: &str) {
    let start_worker = {
        let mut state = lock(&shared.state);
        let Some(row) = state.transfers.get_mut(id) else {
            return;
        };
        if row.running {
            return;
        }
        row.running = true;
        row.attempt_after = None;
        row.backoff = *lock(&shared.backoff_min);
        state.queue.push_back(id.to_owned());
        let free = state.workers < MAX_WORKERS;
        if free {
            state.workers += 1;
        }
        free
    };
    // A worker resting out somebody else's backoff looks again.
    shared.wake.notify_all();
    if start_worker {
        let shared_for_thread = Arc::clone(shared);
        shared.keep(std::thread::spawn(move || worker(&shared_for_thread)));
    }
}

/// Take transfers from the queue until there is nothing left to do.
fn worker(shared: &Arc<Shared>) {
    loop {
        if shared.stopping() {
            let mut state = lock(&shared.state);
            state.workers = state.workers.saturating_sub(1);
            return;
        }
        // `next_job` gives this worker back when the queue holds nothing.
        let Some(job) = next_job(shared) else {
            return;
        };
        match job {
            Job::Ready(id) => run_once(shared, &id),
            Job::Wait(left) => {
                shared.rest(left);
            }
        }
    }
}

/// The next transfer to attempt, or how long until one is ready.
///
/// Returns `None` when the queue is empty, and gives the worker back under
/// the same lock, so a transfer queued a moment later starts a new one.
fn next_job(shared: &Arc<Shared>) -> Option<Job> {
    let now = Instant::now();
    let mut state = lock(&shared.state);
    let mut ready: Option<usize> = None;
    let mut soonest: Option<Duration> = None;
    for (index, id) in state.queue.iter().enumerate() {
        // A transfer whose row has gone is taken out by attempting it: the
        // attempt finds nothing and the queue is one shorter.
        let left = state
            .transfers
            .get(id)
            .and_then(|row| row.attempt_after)
            .map(|at| at.saturating_duration_since(now));
        match left {
            None => {
                ready = Some(index);
                break;
            }
            Some(left) if left.is_zero() => {
                ready = Some(index);
                break;
            }
            Some(left) => soonest = Some(soonest.map_or(left, |best: Duration| best.min(left))),
        }
    }
    if let Some(index) = ready {
        return state.queue.remove(index).map(Job::Ready);
    }
    if let Some(left) = soonest {
        return Some(Job::Wait(left));
    }
    state.workers = state.workers.saturating_sub(1);
    None
}

/// One attempt, and what follows from how it ended.
fn run_once(shared: &Arc<Shared>, id: &str) {
    match attempt(shared, id) {
        Outcome::Done => {
            finish(shared, id, TransferState::Done, None);
            // The record's only job was to survive a restart, and there is
            // nothing left to survive.
            drop(remove_record(shared, id));
            clear_running(shared, id);
        }
        Outcome::Fatal(error) => {
            finish(shared, id, TransferState::Failed, Some(error));
            clear_running(shared, id);
        }
        Outcome::Retry(error) => {
            finish(shared, id, TransferState::Paused, Some(error));
            if shared.stopping() {
                clear_running(shared, id);
                return;
            }
            requeue(shared, id);
        }
    }
}

/// Put a transfer back in the queue, to be tried again after its backoff.
fn requeue(shared: &Arc<Shared>, id: &str) {
    let mut state = lock(&shared.state);
    let Some(row) = state.transfers.get_mut(id) else {
        return;
    };
    row.attempt_after = Some(Instant::now() + row.backoff);
    row.backoff = (row.backoff * 2).min(BACKOFF_MAX);
    state.queue.push_back(id.to_owned());
}

/// Note that no worker owns this transfer any more.
fn clear_running(shared: &Arc<Shared>, id: &str) {
    lock(&shared.state)
        .transfers
        .entry(id.to_owned())
        .and_modify(|row| {
            row.running = false;
            row.attempt_after = None;
        });
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
    notify(shared, Change::Transfers);
    notify(shared, Change::Devices);
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
        // `stop` takes the shared root away. That is not a fault in the
        // transfer, so it pauses rather than fails.
        if shared.stopping() {
            return Outcome::Retry(failed("Runtime::NotReachable"));
        }
        return Outcome::Fatal(failed("Runtime::NotStarted"));
    };

    let (stream, addr, via) = match dial(shared, &plan.device_key_hex, &plan.peer) {
        Ok(found) => found,
        Err(error) => return Outcome::Retry(error),
    };
    mark_reachable(shared, &plan.device_key_hex, addr, via);

    // A cut a test armed is for this dial only. Taking it here, right after
    // the dial it belongs to, means the dial after this one starts clean.
    let cut_after = lock(&shared.cut).take();
    let stream = Cut::new(stream, cut_after, Arc::clone(&shared.wire_bytes));
    // The wrapper fails the next read once `stop` runs, so a transfer does
    // not hold `stop` for a whole file.
    let mut stream = StopAware::new(stream, Arc::clone(&shared.stopping));
    if let Err(error) = exchange_hello(&mut stream, &shared.display_name, this_devices_kind()) {
        return Outcome::Retry(from_rpc(&error));
    }
    {
        let mut state = lock(&shared.state);
        if let Some(row) = state.transfers.get_mut(id) {
            row.state = TransferState::Active;
            row.transport = Some(via);
            row.error = None;
            // A link that works starts the backoff again from the shortest
            // wait, so a long transfer that drops now and then is not
            // punished for having lived a long time.
            row.backoff = *lock(&shared.backoff_min);
        }
    }
    notify(shared, Change::Transfers);

    let mut client = Client::new(stream);
    let record = match load_or_build(shared, id, &plan, fs.as_ref(), &mut client) {
        Ok(record) => record,
        Err(outcome) => return outcome,
    };
    verify_and_land(shared, id, &plan, fs.as_ref(), &mut client, &record)
}

/// Find a way to reach the device, best path first.
///
/// Shared by a transfer attempt and by `Engine::list`, so a dial only has
/// one implementation.
pub(crate) fn dial(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    peer: &PublicKey,
) -> Result<(ferry_core::noise::SecureStream, SocketAddr, Transport), FerryError> {
    for (addr, via) in dial_targets(shared, device_key_hex) {
        if shared.stopping() {
            break;
        }
        if let Ok(connection) = tcp::connect(addr, &shared.key, peer) {
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
    match read_record(&shared.record_path(id)) {
        Ok(Some(Record::Ready(record))) => Ok(record),
        Ok(Some(Record::FirstPass(pass))) => first_pass(shared, id, plan, fs, client, Some(&pass)),
        Ok(None) => first_pass(shared, id, plan, fs, client, None),
        Err(error) => Err(Outcome::Fatal(error)),
    }
}

/// Read the whole file once, building the manifest as the bytes arrive.
fn first_pass<S: Read + Write>(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    fs: &dyn FileOps,
    client: &mut Client<S>,
    saved: Option<&FirstPass>,
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
    let chunk = *lock(&shared.chunk_size);
    // A record from an earlier run says how much was hashed, and no more of
    // the partial file than that is believed.
    let verified = if changed {
        None
    } else {
        saved.map(FirstPass::bytes_done)
    };
    let start = start_offset(fs, &temporary, chunk, changed, verified)
        .map_err(|e| Outcome::Fatal(from_op(e)))?;

    let mut pass = Pass {
        record: shared.record_path(id),
        state: FirstPass {
            source: plan.source.clone(),
            destination: plan.destination.clone(),
            source_size: size,
            source_mtime: entry.modified_unix_secs,
            chunk_size: chunk.get(),
            chunks_done: chunks_in(start, chunk),
        },
        builder: ManifestBuilder::new(chunk),
    };
    // The record goes down before the first byte is asked for. A pass with
    // no record leaves a partial file that nothing knows about.
    write_record(&pass.record, &Record::FirstPass(pass.state.clone())).map_err(Outcome::Fatal)?;

    rehash_local(fs, &temporary, chunk, start, &mut pass.builder)
        .map_err(|e| Outcome::Fatal(from_op(e)))?;
    let span = Span {
        chunk,
        start,
        size,
        temporary: temporary.clone(),
    };
    fetch_rest(shared, id, plan, fs, client, &span, &mut pass)?;

    let record = Transfer::new(
        pass.builder.finish(),
        plan.source.clone(),
        plan.destination.clone(),
    )
    .map_err(|e| Outcome::Fatal(from_transfer(&e)))?;
    let landing = record
        .temporary_path()
        .map_err(|e| Outcome::Fatal(from_transfer(&e)))?;
    fs.rename(&temporary, &landing)
        .map_err(|e| Outcome::Fatal(from_op(e)))?;
    write_record(&shared.record_path(id), &Record::Ready(record.clone()))
        .map_err(Outcome::Fatal)?;
    Ok(record)
}

/// How many whole chunks a byte count holds.
fn chunks_in(bytes: u64, chunk: ChunkSize) -> u32 {
    u32::try_from(bytes / chunk.as_u64()).unwrap_or(u32::MAX)
}

/// Everything the first pass writes to as it runs.
struct Pass {
    /// Where this transfer's record lives.
    record: PathBuf,
    /// The record as it stands, rewritten at every chunk boundary.
    state: FirstPass,
    /// The manifest being built out of the bytes as they arrive.
    builder: ManifestBuilder,
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
    pass: &mut Pass,
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
            Err(Fetch::Rpc(error)) => return Err(classify_rpc(&error)),
            // A peer that answers a chunk in crumbs is not answering. The
            // attempt pauses and the backoff decides when to try again.
            Err(Fetch::Stalled) => {
                return Err(Outcome::Retry(failed("TransferError::ShortRead")));
            }
        };
        if bytes.len() != usize::try_from(want).unwrap_or(usize::MAX) {
            // The device holds less of the file than it said it holds.
            return Err(Outcome::Fatal(failed("TransferError::ShortRead")));
        }
        write_all_local(fs, &span.temporary, offset, &bytes)
            .map_err(|e| Outcome::Fatal(from_op(e)))?;
        pass.builder.push(&bytes);
        offset += u64::from(want);
        // The record is rewritten at every chunk boundary, so a first pass
        // that stops here starts again from this point and not from zero.
        pass.state.chunks_done = chunks_in(offset, span.chunk);
        write_record(&pass.record, &Record::FirstPass(pass.state.clone()))
            .map_err(Outcome::Fatal)?;
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
/// exactly one chunk long. `verified` is what the stored record says was
/// hashed, and nothing past it is believed, whatever the file holds.
fn start_offset(
    fs: &dyn FileOps,
    temporary: &RemotePath,
    chunk: ChunkSize,
    changed: bool,
    verified: Option<u64>,
) -> Result<u64, OpError> {
    let held = match fs.stat(temporary) {
        Ok(entry) => entry.size,
        Err(OpError::NotFound) => return Ok(0),
        Err(other) => return Err(other),
    };
    let mut keep = if changed {
        0
    } else {
        (held / chunk.as_u64()) * chunk.as_u64()
    };
    if let Some(verified) = verified {
        keep = keep.min(verified);
    }
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

/// Why one chunk did not arrive.
enum Fetch {
    /// The call itself failed.
    Rpc(RpcError),
    /// The peer answered, and answered, and never finished the chunk.
    Stalled,
}

/// Read one range from the peer, in pieces one message can carry.
///
/// A peer that answers one byte per read would keep this loop going for a
/// mebibyte of round trips, holding a worker with no progress worth the
/// name. Two bounds end it: the number of reads one chunk may take, and the
/// time one chunk may take. Whichever comes first ends the attempt, and the
/// backoff decides when to try again.
fn fetch_remote<S: Read + Write>(
    client: &mut Client<S>,
    path: &RemotePath,
    offset: u64,
    want: u32,
) -> Result<Vec<u8>, Fetch> {
    let started = Instant::now();
    let mut reads: u32 = 0;
    let mut out: Vec<u8> = Vec::new();
    while let Some(piece) = next_piece(out.len(), want) {
        reads += 1;
        if reads > limits::MAX_READS_PER_CHUNK || started.elapsed() > CHUNK_DEADLINE {
            return Err(Fetch::Stalled);
        }
        let at = offset + u64::try_from(out.len()).unwrap_or(0);
        let got = client.read(path, at, piece).map_err(Fetch::Rpc)?;
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
            notify(self.shared, Change::Transfers);
            notify(self.shared, Change::Devices);
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
