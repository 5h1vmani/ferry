//! Transfers, on a small pool of threads, until each is done or fails.
//!
//! # Why the first pass fetches the manifest first
//!
//! `session::pull` needs a manifest before it starts. Before
//! `docs/engine-contract.md` item 16a, no operation asked a peer for one, so
//! the first pass built its own from whatever bytes arrived, trusting them
//! until a later resume proved otherwise.
//!
//! `Request::Manifest` closes that gap. The first pass now fetches the
//! peer's manifest before asking for the first chunk, and verifies every
//! chunk against it as that chunk lands: the same `Manifest::verify_chunk`
//! check an ordinary resume already runs. A chunk that fails stops the
//! attempt at once, with `TransferError::ChunkFailedVerification` and the
//! failing index, instead of only being caught on a later connection.
//!
//! The pass still moves the file in one pass over the network: for each
//! chunk it reads, verifies, and writes before asking for the next one.
//! There is no second pass that re-reads what the first pass already wrote.
//!
//! Finding where to resume an interrupted first pass reuses
//! `session::resume_point`: the partial file is read back and hashed
//! against the manifest just fetched, never trusted from a stored record,
//! per `docs/protocol.md` section 9 ("the manifest is a hint, the disk is
//! the truth"). Once the whole file verifies, the fetched manifest becomes
//! the record's own manifest, and every later attempt is an ordinary
//! `session::pull` that resumes from it.
//!
//! A first pass writes a record of its own at every chunk boundary, holding
//! the size of the source and how many whole chunks have verified. So a
//! first pass that is cut short is picked up where it stopped, even after
//! the app has closed and opened again. Without that record the partial
//! file was left in the person's folder and the whole file was fetched
//! again.
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

use ferry_core::chunk::Manifest;
use ferry_core::limits;
use ferry_core::noise::PublicKey;
use ferry_core::ops::{FileKind, OpError};
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, RpcError, exchange_hello};
use ferry_core::session::{Progress, Transfer, TransferError, pull_with_progress, resume_point};
use ferry_core::tcp;
use ferry_core::wire::WireError;

use crate::access::{self, EntryFields};
use crate::batch::{self, BatchRecord};
use crate::engine::{
    Shared, SocketRegistration, dial_targets, mark_reachable, notify, remove_record,
};
use crate::errors::{failed, from_op, from_rpc, from_transfer};
use crate::guard::{Cut, StopAware};
use crate::notify::Change;
use crate::record::{FirstPass, Meta, Record, read_record, write_record};
use crate::state::{key_from_hex, lock, now_unix_secs};
use crate::{Direction, FerryError, Origin, TransferState, Transport};

/// The shortest wait before trying again.
pub(crate) const BACKOFF_MIN: Duration = Duration::from_secs(1);

/// The longest wait before trying again.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How often the app is told a transfer moved.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// How long a transfer's own `speed_bytes_per_sec` is measured over.
const TRANSFER_SPEED_WINDOW: Duration = Duration::from_secs(2);

/// How many transfers may move at once.
const MAX_WORKERS: usize = 4;

/// How long one chunk may take before the peer counts as stalled.
const CHUNK_DEADLINE: Duration = Duration::from_secs(30);

/// How one attempt ended.
pub(crate) enum Outcome {
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
    let mut held_write: Option<(String, String, u64, i64)> = None;
    let mut auto_copy_update: Option<(String, u32)> = None;
    let batch_update = {
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
        if matches!(state, TransferState::Done | TransferState::Failed) {
            row.ended_unix_secs = Some(now_unix_secs());
        }
        let bytes_total = row.bytes_total;
        let batch_id = row.batch_id.clone();

        // docs/engine-contract.md item 14: every completed pull, manual or
        // automatic, writes a row to the held index. `source_size` and
        // `source_mtime` are always set by the time a pull reaches `Done`:
        // `first_pass`, above, sets both before the first chunk is asked
        // for.
        if state == TransferState::Done
            && row.direction == Direction::Pull
            && let (Some(size), Some(mtime)) = (row.source_size, row.source_mtime)
        {
            held_write = Some((
                row.device_key_hex.clone(),
                row.source.as_str().to_owned(),
                size,
                mtime,
            ));
        }

        // docs/engine-contract.md, batch D, item 2, and the D4 fix: this
        // row's own record is removed right after this, by the caller, once
        // it is `Done`, so a restart would not see it to count again. The
        // batch keeps a floor of its own, raised here and written down,
        // that `BatchRow::info` reports at least, once this row is gone.
        let batch_update = if state == TransferState::Done {
            batch_id.clone().and_then(|batch_id| {
                let batch = locked.batches.get_mut(&batch_id)?;
                batch.done_files += 1;
                batch.done_bytes += bytes_total;
                Some((shared.batch_path(&batch_id), BatchRecord::of(batch)))
            })
        } else {
            None
        };

        // docs/engine-contract.md item 14: once every transfer in an
        // automatic batch has reached `Done` or `Failed`, the batch has
        // ended, and the run it came from is recorded. `BatchRow::info`
        // already knows how to tell: `ended_unix_secs` is `Some` exactly
        // once none of its transfers is still `Queued`, `Active` or
        // `Paused`.
        if matches!(state, TransferState::Done | TransferState::Failed)
            && let Some(batch_id) = &batch_id
            && let Some(batch) = locked.batches.get(batch_id)
            && batch.origin == Origin::Automatic
        {
            let info = batch.info(&locked.transfers);
            if info.ended_unix_secs.is_some() {
                auto_copy_update = Some((batch.device_key_hex.clone(), info.files_done));
            }
        }

        batch_update
    };
    if let Some((path, record)) = batch_update {
        // The record is best effort here, the same as every other batch
        // write: a failed write leaves the floor exactly where it was, and
        // the live count still fills in for this run.
        drop(batch::write_batch(&path, &record));
    }
    if let Some((device_key_hex, source_path, size, mtime)) = held_write {
        record_held_row(shared, id, &device_key_hex, &source_path, size, mtime);
    }
    if let Some((device_key_hex, files_done)) = auto_copy_update {
        crate::auto_copy::record_run(shared, &device_key_hex, now_unix_secs(), files_done);
    }
    notify(shared, Change::Transfers);
    notify(shared, Change::Devices);
}

/// Write one row to the held index for a pull that just reached `Done`.
///
/// The record this transfer finished with still holds the manifest whose
/// root hash the row needs; `run_once` removes that record right after
/// `finish` returns. Best effort, the same as the batch record write above:
/// a failed write here costs one file copied again, never a wrong answer.
fn record_held_row(
    shared: &Arc<Shared>,
    id: &str,
    device_key_hex: &str,
    source_path: &str,
    size: u64,
    mtime: i64,
) {
    let Ok(Some(Record::Ready(_meta, transfer))) = read_record(&shared.record_path(id)) else {
        return;
    };
    let mut held = lock(&shared.held);
    // G1: `record` appends this one row to disk itself, so there is no
    // separate whole-file save to call here any more.
    drop(held.record(crate::held::HeldRow {
        device_key_hex: device_key_hex.to_owned(),
        source_path: source_path.to_owned(),
        size,
        mtime,
        root: *transfer.manifest.root().as_bytes(),
    }));
}

/// What one attempt needs to know, copied out from under the lock.
///
/// Shared by a pull's own attempt logic below and by `push::attempt`. For a
/// pull, `source` is the file's path on the peer and `destination` is where
/// it lands here, relative to the download folder. For a push, the meaning
/// flips, per `docs/engine-contract.md` item 5: `source` is this device's
/// local file, stored with its leading slash stripped so it fits the same
/// `RemotePath` type, and `destination` is the file's path on the peer.
pub(crate) struct Plan {
    pub(crate) device_key_hex: String,
    pub(crate) peer: PublicKey,
    pub(crate) source: RemotePath,
    pub(crate) destination: RemotePath,
    /// The 32 hex characters that name this transfer's own partial file
    /// during the first pass.
    pub(crate) suffix: String,
    /// When `pull` created this transfer. Written into every record this
    /// attempt writes.
    pub(crate) started_unix_secs: i64,
    /// Which way this transfer moves the file. Written into every record
    /// this attempt writes.
    pub(crate) direction: Direction,
    /// Which batch this transfer belongs to, if `pull_folder` created it.
    /// Written into every record this attempt writes.
    pub(crate) batch_id: Option<String>,
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
        suffix: id.rsplit('-').next().unwrap_or(id).to_owned(),
        started_unix_secs: row.started_unix_secs,
        direction: row.direction,
        batch_id: row.batch_id.clone(),
    })
}

/// One connection, one go at moving the file.
///
/// A pull needs the shared download folder before it dials anyone, because
/// that is where it lands the file. A push reads its own local file through
/// a `LocalFs` it opens fresh on that file's own parent folder
/// (`push::open_local`), so it needs no shared folder at all.
fn attempt(shared: &Arc<Shared>, id: &str) -> Outcome {
    let Some(plan) = plan_for(shared, id) else {
        return Outcome::Fatal(failed("Runtime::TransferNotFound"));
    };
    let fs = if plan.direction == Direction::Pull {
        match shared.download_fs() {
            Some(fs) => Some(fs),
            // `stop` takes the download folder away. That is not a fault in
            // the transfer, so it pauses rather than fails.
            None if shared.stopping() => {
                return Outcome::Retry(failed("Runtime::NotReachable"));
            }
            None => return Outcome::Fatal(failed("Runtime::NotStarted")),
        }
    } else {
        None
    };

    let (stream, socket, addr, via) = match dial(shared, &plan.device_key_hex, &plan.peer) {
        Ok(found) => found,
        Err(error) => return Outcome::Retry(error),
    };
    mark_reachable(shared, &plan.device_key_hex, addr, via);

    // Registered as soon as the connection exists, so `stop` can close it
    // even if this attempt later hangs inside the hello exchange or a file
    // read. The same id doubles as this attempt's access log connection id,
    // below. docs/engine-contract.md item 16c.
    let connection = shared.next_connection_id();
    let _socket = SocketRegistration::new(shared, connection, socket);

    // A cut a test armed is for this dial only. Taking it here, right after
    // the dial it belongs to, means the dial after this one starts clean.
    let cut_after = lock(&shared.cut).take();
    let stream = Cut::new(stream, cut_after, Arc::clone(&shared.wire_bytes));
    // The wrapper fails the next read once `stop` runs, so a transfer does
    // not hold `stop` for a whole file.
    let mut stream = StopAware::new(stream, Arc::clone(&shared.stopping));
    if let Err(error) = exchange_hello(&mut stream, &shared.display_name, shared.kind) {
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

    // Past this point the attempt may read from the peer, so it now has a
    // connection worth the access log's attention. `bytes_before` is this
    // transfer's own running total, which every read past here only ever
    // advances (`Reporter::moved`), so the delta after the attempt is
    // exactly what this attempt itself received (docs/engine-contract.md,
    // item 13).
    let bytes_before = bytes_done_of(shared, id);
    let mut client = Client::new(stream);

    if plan.direction == Direction::Push {
        // A push logs its one access log entry up front, in `push::push` and
        // `push::push_files`, through the existing `record_this`: see
        // `docs/engine-contract.md` item 5. Nothing here logs per attempt.
        return crate::push::attempt(shared, id, &plan, &mut client);
    }

    let fs = fs.expect("checked above: a pull always has a download folder here");
    let record = match load_or_build(shared, id, &plan, fs.as_ref(), &mut client) {
        Ok(record) => record,
        Err(outcome) => {
            record_attempt_read(shared, id, &plan, connection, bytes_before);
            return outcome;
        }
    };
    let outcome = verify_and_land(shared, id, &plan, fs.as_ref(), &mut client, &record);
    record_attempt_read(shared, id, &plan, connection, bytes_before);
    outcome
}

/// This transfer's own `bytes_done`, right now.
///
/// Shared with `push.rs`, which reads it the same way `attempt` does above.
pub(crate) fn bytes_done_of(shared: &Arc<Shared>, id: &str) -> u64 {
    lock(&shared.state)
        .transfers
        .get(id)
        .map_or(0, |row| row.bytes_done)
}

/// Record what this attempt received from the peer, as actor `This`, and
/// end the roll-up's connection for it. Skips logging when nothing was
/// received: a dial that never reached the file layer, or one that failed
/// before a byte arrived, has nothing to report. Also skips logging when
/// `plan.batch_id` is `Some`: a file copied as part of a folder copy logs
/// nothing of its own on the calling side, because `pull_folder`'s own
/// entry, with its `files` and `bytes` totals, already covers it
/// (docs/engine-contract.md, item 13, "Rolling up").
fn record_attempt_read(
    shared: &Arc<Shared>,
    id: &str,
    plan: &Plan,
    connection: u64,
    bytes_before: u64,
) {
    if plan.batch_id.is_some() {
        return;
    }
    let received = bytes_done_of(shared, id).saturating_sub(bytes_before);
    if received == 0 {
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
        EntryFields {
            device_key_hex: plan.device_key_hex.clone(),
            actor: access::Actor::This,
            verb: access::AccessVerb::Read,
            path: plan.source.as_str().to_owned(),
            bytes: Some(received),
            entries: None,
            files: None,
        },
    );
    rollup.connection_ended(now, connection);
}

/// Find a way to reach the device, best path first.
///
/// Shared by a transfer attempt, `Engine::list`, and `Engine::pull_folder`,
/// so a dial only has one implementation.
///
/// The raw socket travels alongside the stream so the caller can register it
/// with `Shared`, for `stop` to close directly (`docs/engine-contract.md`
/// item 16c).
pub(crate) fn dial(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    peer: &PublicKey,
) -> Result<
    (
        ferry_core::noise::SecureStream,
        std::net::TcpStream,
        SocketAddr,
        Transport,
    ),
    FerryError,
> {
    for (addr, via) in dial_targets(shared, device_key_hex) {
        if shared.stopping() {
            break;
        }
        if let Ok(connection) = tcp::connect(addr, &shared.key, peer) {
            return Ok((connection.stream, connection.socket, connection.remote, via));
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
        // The meta an earlier attempt of this same row wrote is not needed
        // again: the row it came from is this attempt's own plan. Nor is the
        // rest of a saved `FirstPass`: the manifest fetched below is what
        // decides where to resume, not the stored record.
        Ok(Some(Record::Ready(_meta, record))) => Ok(record),
        Ok(Some(Record::FirstPass(..)) | None) => first_pass(shared, id, plan, fs, client),
        Err(error) => Err(Outcome::Fatal(error)),
    }
}

/// Fetch the peer's manifest, then fetch and verify every chunk it has not
/// already verified, writing the partial file in the same pass.
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
    // Fetched before the first chunk, so every chunk below is checked
    // against it as it lands, rather than trusted on arrival.
    // docs/engine-contract.md item 16a.
    let manifest = match client.manifest(&plan.source) {
        Ok(manifest) => manifest,
        Err(error) => return Err(classify_rpc(&error)),
    };
    let size = manifest.length();
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

    // The manifest is a hint, the disk is the truth (docs/protocol.md
    // section 9): whatever the partial file already holds is re-verified
    // against the manifest just fetched, never trusted from a stored
    // record. This is the same resume `session::pull` uses once a manifest
    // exists, reused here now that the first pass has one too.
    let start_index =
        resume_point(fs, &temporary, &manifest).map_err(|e| Outcome::Fatal(from_transfer(&e)))?;

    let mut pass = Pass {
        record: shared.record_path(id),
        state: FirstPass {
            source: plan.source.clone(),
            destination: plan.destination.clone(),
            source_size: size,
            source_mtime: entry.modified_unix_secs,
            chunk_size: manifest.chunk_size().get(),
            chunks_done: u32::try_from(start_index).unwrap_or(u32::MAX),
        },
        // The record is only ever written while this attempt is in
        // progress, so its end time is never anything but `None`.
        meta: Meta {
            started_unix_secs: plan.started_unix_secs,
            ended_unix_secs: None,
            direction: plan.direction,
            batch_id: plan.batch_id.clone(),
        },
    };
    // The record goes down before the first byte is asked for. A pass with
    // no record leaves a partial file that nothing knows about.
    write_record(
        &pass.record,
        &Record::FirstPass(pass.meta.clone(), pass.state.clone()),
    )
    .map_err(Outcome::Fatal)?;

    let span = Span {
        start_index,
        manifest,
        temporary: temporary.clone(),
    };
    fetch_rest(shared, id, plan, fs, client, &span, &mut pass)?;

    let record = Transfer::new(span.manifest, plan.source.clone(), plan.destination.clone())
        .map_err(|e| Outcome::Fatal(from_transfer(&e)))?;
    let landing = record
        .temporary_path()
        .map_err(|e| Outcome::Fatal(from_transfer(&e)))?;
    fs.rename(&temporary, &landing)
        .map_err(|e| Outcome::Fatal(from_op(e)))?;
    write_record(
        &shared.record_path(id),
        &Record::Ready(pass.meta.clone(), record.clone()),
    )
    .map_err(Outcome::Fatal)?;
    Ok(record)
}

/// Everything the first pass writes to as it runs.
struct Pass {
    /// Where this transfer's record lives.
    record: PathBuf,
    /// The record as it stands, rewritten at every chunk boundary.
    state: FirstPass,
    /// The start time and direction written into every record this attempt
    /// writes. The end time is always `None`: the record is written only
    /// while the transfer is in progress.
    meta: Meta,
}

/// Where the first pass writes, and how far it has to go.
struct Span {
    /// The chunk index to resume from, found by re-verifying the partial
    /// file against `manifest`.
    start_index: usize,
    /// The peer's manifest for this file, fetched before the first chunk.
    manifest: Manifest,
    /// The partial file this pass writes to.
    temporary: RemotePath,
}

/// Fetch every chunk the partial file does not already hold, verifying each
/// one against the peer's manifest as it lands.
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
    let manifest = &span.manifest;
    if manifest.length() == 0 {
        // An empty file still needs its partial file to exist, because the
        // pull that follows truncates and renames it.
        fs.write(&span.temporary, 0, &[])
            .map_err(|e| Outcome::Fatal(from_op(e)))?;
    }
    for index in span.start_index..manifest.chunk_count() {
        let Some((offset, length)) = manifest.chunk_range(index) else {
            break;
        };
        let bytes = match fetch_remote(client, &plan.source, offset, length) {
            Ok(bytes) => bytes,
            Err(Fetch::Rpc(error)) => return Err(classify_rpc(&error)),
            // A peer that answers a chunk in crumbs is not answering. The
            // attempt pauses and the backoff decides when to try again.
            Err(Fetch::Stalled) => {
                return Err(Outcome::Retry(failed("TransferError::ShortRead")));
            }
        };
        if bytes.len() != usize::try_from(length).unwrap_or(usize::MAX) {
            // The device holds less of the file than its manifest said it
            // holds.
            return Err(Outcome::Fatal(failed("TransferError::ShortRead")));
        }
        if !manifest.verify_chunk(index, &bytes) {
            return Err(Outcome::Fatal(from_transfer(
                &TransferError::ChunkFailedVerification { index },
            )));
        }
        write_all_local(fs, &span.temporary, offset, &bytes)
            .map_err(|e| Outcome::Fatal(from_op(e)))?;
        // The record is rewritten at every chunk boundary, so a first pass
        // that stops here starts again from this point and not from zero.
        pass.state.chunks_done = u32::try_from(index + 1).unwrap_or(u32::MAX);
        write_record(
            &pass.record,
            &Record::FirstPass(pass.meta.clone(), pass.state.clone()),
        )
        .map_err(Outcome::Fatal)?;
        reporter.moved(offset + u64::from(length), manifest.length());
        if shared.stopping() {
            return Err(Outcome::Retry(failed("Runtime::NotReachable")));
        }
    }
    // A resumed file may hold more than the manifest if an earlier attempt,
    // or an earlier version of the source file, left bytes past the end.
    fs.truncate(&span.temporary, manifest.length())
        .map_err(|e| Outcome::Fatal(from_op(e)))?;
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

/// A connection failure is worth another go. A refusal by the peer is not,
/// and neither is a manifest whose own chunks do not merge to its stated
/// root hash: that peer will serve the same broken manifest again.
///
/// Shared with `push.rs`: the same rule decides whether a failed call during
/// a push is worth retrying.
pub(crate) fn classify_rpc(error: &RpcError) -> Outcome {
    match error {
        RpcError::Remote(inner) => Outcome::Fatal(from_op(*inner)),
        RpcError::Wire(WireError::BadManifest) => Outcome::Fatal(from_rpc(error)),
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

/// Where the first pass writes, before the whole file has verified.
fn temp_path(destination: &RemotePath, suffix: &str) -> Result<RemotePath, FerryError> {
    RemotePath::parse(&format!("{}.{suffix}.part", destination.as_str()))
        .map_err(crate::errors::from_path)
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
///
/// Shared with `push.rs`: a push reports its own progress the same way a
/// pull does, through the same fields on the same `TransferRow`.
pub(crate) struct Reporter<'a> {
    shared: &'a Arc<Shared>,
    id: String,
    device_key_hex: String,
    last_report: Instant,
    bytes_at_last_report: u64,
    /// The start of the window `TransferInfo::speed_bytes_per_sec` is
    /// measured over, and the bytes done at that moment.
    speed_window_start: Instant,
    bytes_at_speed_window_start: u64,
    /// `bytes_at_last_report` and `bytes_at_speed_window_start` are not
    /// known until [`Reporter::moved`] is first called: a resumed transfer's
    /// first `bytes_done` already counts bytes a previous attempt verified,
    /// and `verify_and_land`'s first call counts bytes just reverified
    /// locally, not bytes this attempt moved over the wire. Seeding both
    /// marks at zero would count all of that as freshly moved. `false` until
    /// that first call seeds them from the `bytes_done` it sees.
    seeded: bool,
}

impl<'a> Reporter<'a> {
    /// Start reporting for one transfer.
    pub(crate) fn new(shared: &'a Arc<Shared>, id: &str, device_key_hex: &str) -> Self {
        let now = Instant::now();
        Self {
            shared,
            id: id.to_owned(),
            device_key_hex: device_key_hex.to_owned(),
            last_report: now,
            bytes_at_last_report: 0,
            speed_window_start: now,
            bytes_at_speed_window_start: 0,
            seeded: false,
        }
    }

    /// Note that the transfer has reached `bytes_done` of `total`.
    pub(crate) fn moved(&mut self, bytes_done: u64, total: u64) {
        if !self.seeded {
            self.bytes_at_last_report = bytes_done;
            self.bytes_at_speed_window_start = bytes_done;
            self.seeded = true;
        }
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

        // The transfer's own speed is measured over a longer window than the
        // device-level figure above, so a brief stall does not make it jump
        // around.
        let window_elapsed = self.speed_window_start.elapsed();
        let transfer_speed = (window_elapsed >= TRANSFER_SPEED_WINDOW).then(|| {
            let moved = bytes_done.saturating_sub(self.bytes_at_speed_window_start);
            moved / window_elapsed.as_secs().max(1)
        });

        {
            let mut state = lock(&self.shared.state);
            if let Some(row) = state.transfers.get_mut(&self.id) {
                row.bytes_done = bytes_done;
                row.bytes_total = total;
                if let Some(transfer_speed) = transfer_speed {
                    row.speed_bytes_per_sec = Some(transfer_speed);
                }
            }
            if due {
                state.live_mut(&self.device_key_hex).speed_bytes_per_sec = speed;
            }
        }
        if transfer_speed.is_some() {
            self.speed_window_start = Instant::now();
            self.bytes_at_speed_window_start = bytes_done;
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

#[cfg(test)]
mod tests {
    use super::{Outcome, classify_rpc};
    use ferry_core::rpc::RpcError;
    use ferry_core::wire::WireError;

    #[test]
    fn a_self_inconsistent_manifest_is_fatal_not_worth_retrying() {
        // F2: a manifest whose own chunks do not merge to its stated root
        // hash is not a connection hiccup; the same peer will serve the
        // same broken manifest again, so retrying it can never succeed.
        //
        // `Manifest::from_parts` refuses to build a value like this at all,
        // which is exactly what keeps every real `FileOps` implementation
        // from ever handing `classify_rpc` one: the only way this error
        // reaches it is a peer that does not speak the protocol honestly at
        // the wire level, below the type that makes an inconsistent
        // manifest unrepresentable. This test exercises the classification
        // rule directly, at the boundary that error crosses.
        let error = RpcError::Wire(WireError::BadManifest);
        assert!(
            matches!(classify_rpc(&error), Outcome::Fatal(_)),
            "a self-inconsistent manifest must not be retried"
        );
    }

    #[test]
    fn a_frame_layer_hiccup_is_still_worth_retrying() {
        // The fix for F2 narrows only `WireError::BadManifest`; every other
        // wire or frame failure is still ordinary connection trouble.
        let error = RpcError::Wire(WireError::UnexpectedEnd);
        assert!(
            matches!(classify_rpc(&error), Outcome::Retry(_)),
            "a truncated read is a connection problem, not a broken peer"
        );
    }
}
