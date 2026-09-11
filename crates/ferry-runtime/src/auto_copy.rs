//! Automatic copying: job 7, `docs/engine-contract.md` item 14.
//!
//! When a device's automatic copy is on, this device copies every new file
//! under that device's camera folder to itself, without anyone asking. One
//! way, additive: it never deletes anything and never writes back.
//!
//! # The stored setting
//!
//! One row per device, in `data_dir/auto_copy`, holding `enabled`,
//! `last_run_unix_secs` and `last_run_files`. `source` and `destination` are
//! never stored: `source` is learned fresh from the peer's own root list
//! each time a run starts, and `destination` is always the download folder
//! plus `/DCIM`.
//!
//! # The run
//!
//! [`on_became_reachable`], [`set_enabled`] and [`run_for_every_reachable_enabled`]
//! are the three triggers `docs/engine-contract.md` names: the transition
//! from not reachable to reachable, turning the switch on for a device that
//! is already reachable, and every reachable, enabled device once at
//! `Engine::start`. All three end up at [`maybe_spawn_run`], which checks
//! the stored setting and the one-run-per-device guard, then hands off to a
//! thread of its own, kept in `shared.joins` so `stop` waits for it.
//!
//! That thread dials the peer, lists its first root, lists `<root>/DCIM`
//! recursively with the same bounds `folder.rs` gives `pull_folder`, and
//! then decides what is new: a file already named in the held index, by
//! device, path, size and modified time, is skipped without asking the peer
//! anything else. For the rest, the peer's manifest is fetched, and a root
//! hash already in the held index is skipped too, wherever it was pulled
//! from. What remains is queued as one batch with `Origin::Automatic`, the
//! same way `Engine::pull_folder` queues a folder a person asked for, and
//! the thread's job ends there: the transfer pool moves the files.
//!
//! Once every transfer in that batch reaches `Done` or `Failed`,
//! `transfer::finish` calls [`record_run`] with the batch's own file count.
//! A run that finds nothing to queue records that directly instead, since no
//! batch exists to watch end.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ferry_core::localfs::LocalFs;
use ferry_core::path::RemotePath;
use ferry_core::rpc::{Client, FileOps, exchange_hello};
use ferry_core::session::SessionId;
use ferry_core::wire::{Decoder, Encoder};

use crate::batch::{self, BatchRecord};
use crate::engine::{Shared, SocketRegistration, leaf_of, mark_reachable, notify};
use crate::errors::failed;
use crate::folder;
use crate::guard::StopAware;
use crate::notify::Change;
use crate::state::{BatchRow, TransferRow, key_from_hex, lock, now_unix_secs};
use crate::transfer::{self, BACKOFF_MIN};
use crate::{AutoCopy, Direction, FerryError, Origin, TransferState};

/// The newest format this build writes, and the only one it reads.
const FORMAT_VERSION: u8 = 1;

/// The most devices this store holds a row for. A device can only gain a row
/// once it is paired, and pairing itself never holds more than this many
/// devices (`ferry_core::peers`), so this only ever bounds a corrupt or
/// hostile file.
const MAX_DEVICES: usize = 64;

/// The most bytes a device key, in hex, may take.
const MAX_DEVICE_KEY_LEN: usize = 64;

/// One device's stored automatic copy setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AutoCopyRow {
    pub(crate) enabled: bool,
    pub(crate) last_run_unix_secs: Option<i64>,
    pub(crate) last_run_files: Option<u32>,
}

/// Every device's automatic copy setting, persisted to one file.
#[derive(Debug, Clone)]
pub(crate) struct AutoCopyStore {
    path: PathBuf,
    rows: BTreeMap<String, AutoCopyRow>,
    /// Serializes `record_run` and `set_enabled` against each other.
    ///
    /// `docs/audits/fable-lifecycle.md`, finding 7: both used to hold
    /// `shared.auto_copy` across `save`, which calls `fsync`, so a
    /// `SwiftUI` read of `Engine::auto_copy` on the main actor waited on
    /// that write.
    /// The fix is the same shape as `engine.rs`'s own `save_peers`: clone
    /// the store, change and save the clone with `shared.auto_copy` not
    /// held, then swap it in under a brief second hold. That leaves a gap
    /// where two writers could both clone the same starting point and the
    /// second `save` to land would silently lose the first writer's change,
    /// so this lock is held across each writer's whole clone-change-save-
    /// swap sequence instead, the same job `save_peers`'s own `peers_write`
    /// does. Sharing one `Arc` across every clone, rather than adding a
    /// field to `Shared` alongside `auto_copy`, is what keeps this fix
    /// inside this file.
    write_lock: Arc<Mutex<()>>,
}

impl AutoCopyStore {
    /// Load the store from `path`.
    ///
    /// A missing file, or one that fails to decode, comes back as an empty
    /// store: every device answers as disabled and never run, the same as a
    /// device nobody has touched the switch for yet.
    pub(crate) fn load(path: &Path) -> Self {
        let rows = fs::read(path)
            .ok()
            .and_then(|bytes| decode_rows(&bytes))
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            rows,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Write the store out, replacing whatever was there before.
    ///
    /// # Errors
    ///
    /// Returns `TransferError::Local` when local storage refuses the write,
    /// and `TransferError::NoRandomness` when the temporary name cannot be
    /// made.
    pub(crate) fn save(&self) -> Result<(), FerryError> {
        write_private_file(&self.path, &encode_rows(&self.rows))
    }

    pub(crate) fn get(&self, device_key_hex: &str) -> Option<AutoCopyRow> {
        self.rows.get(device_key_hex).copied()
    }

    /// Turn the switch on or off for a device, keeping its last run facts.
    pub(crate) fn set_enabled(&mut self, device_key_hex: &str, enabled: bool) {
        self.rows
            .entry(device_key_hex.to_owned())
            .or_default()
            .enabled = enabled;
    }

    /// Record that a run for this device ended, and how many files it
    /// copied.
    pub(crate) fn record_run(&mut self, device_key_hex: &str, ended_unix_secs: i64, files: u32) {
        let row = self.rows.entry(device_key_hex.to_owned()).or_default();
        row.last_run_unix_secs = Some(ended_unix_secs);
        row.last_run_files = Some(files);
    }
}

fn decode_rows(bytes: &[u8]) -> Option<BTreeMap<String, AutoCopyRow>> {
    let mut d = Decoder::new(bytes);
    let version = d.u8().ok()?;
    if version != FORMAT_VERSION {
        return None;
    }
    let count = d.u32().ok()?;
    if count as usize > MAX_DEVICES {
        return None;
    }
    let mut rows = BTreeMap::new();
    for _ in 0..count {
        let device_key_hex = d.text(MAX_DEVICE_KEY_LEN).ok()?.to_owned();
        let enabled = d.u8().ok()? != 0;
        let last_run_unix_secs = match d.u8().ok()? {
            0 => None,
            _ => Some(decode_i64(d.u64().ok()?)),
        };
        let last_run_files = match d.u8().ok()? {
            0 => None,
            _ => Some(d.u32().ok()?),
        };
        rows.insert(
            device_key_hex,
            AutoCopyRow {
                enabled,
                last_run_unix_secs,
                last_run_files,
            },
        );
    }
    d.finish().ok()?;
    Some(rows)
}

fn encode_rows(rows: &BTreeMap<String, AutoCopyRow>) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(FORMAT_VERSION);
    let count = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    e.u32(count);
    for (device_key_hex, row) in rows {
        e.text(device_key_hex);
        e.u8(u8::from(row.enabled));
        match row.last_run_unix_secs {
            Some(value) => {
                e.u8(1);
                e.u64(encode_i64(value));
            }
            None => {
                e.u8(0);
            }
        }
        match row.last_run_files {
            Some(value) => {
                e.u8(1);
                e.u32(value);
            }
            None => {
                e.u8(0);
            }
        }
    }
    e.finish()
}

fn encode_i64(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

fn decode_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), FerryError> {
    let temporary = temporary_name(path)?;
    let written = write_and_sync(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        drop(fs::remove_file(&temporary));
        return Err(failed("TransferError::Local"));
    }
    Ok(())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = open_new_private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

// G4: create `path` in mode `0o600` on Unix, the same private mode
// `ferry-core`'s `peers.rs` gives its own file, set as part of the same
// syscall that creates it so there is no moment where the file exists with
// a wider mode. `create_new` already refuses to touch anything already
// there, temporary name or not.
#[cfg(unix)]
fn open_new_private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_new_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn temporary_name(path: &Path) -> Result<PathBuf, FerryError> {
    let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{session}.tmp"));
    Ok(PathBuf::from(name))
}

// ---------------------------------------------------------------------------
// The boundary.
// ---------------------------------------------------------------------------

/// `Engine::auto_copy`. Always answers, even for a device that is not
/// paired or has never had the switch touched: there is no way to fail, so
/// an unknown device reads the same as one nobody has configured yet.
pub(crate) fn get(shared: &Arc<Shared>, device_key_hex: &str) -> AutoCopy {
    let row = lock(&shared.auto_copy).get(device_key_hex);
    let (source_root, running) = {
        let state = lock(&shared.state);
        let source_root = state
            .live
            .get(device_key_hex)
            .and_then(|live| live.auto_copy_source_root.clone());
        // G8: derived, not stored. `docs/engine-contract.md` item 14: "a
        // run that finds nothing new records a run and makes no batch; the
        // Running state is `running`." So this is true only while a batch
        // this device's run actually queued is still moving, never for the
        // run's own listing and skip-check, and never past the batch's own
        // end.
        let running = state.batches.values().any(|batch| {
            batch.device_key_hex == device_key_hex
                && batch.origin == Origin::Automatic
                && batch.info(&state.transfers).ended_unix_secs.is_none()
        });
        (source_root, running)
    };
    let source = match source_root {
        Some(root) => format!("{root}/DCIM"),
        // Learned only once a run has asked the peer for its roots. Until
        // then this names the folder without the device's own root prefix,
        // the same placeholder the Mac's own preview data already uses.
        None => "DCIM".to_owned(),
    };
    let destination = format!("{}/DCIM", lock(&shared.download_dir).display());
    AutoCopy {
        device_key_hex: device_key_hex.to_owned(),
        enabled: row.is_some_and(|row| row.enabled),
        source,
        destination,
        last_run_unix_secs: row.and_then(|row| row.last_run_unix_secs),
        last_run_files: row.and_then(|row| row.last_run_files),
        running,
    }
}

/// `Engine::set_auto_copy`.
///
/// # Errors
///
/// Returns `Runtime::NotPaired` when no device has that key.
pub(crate) fn set_enabled(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    enabled: bool,
) -> Result<(), FerryError> {
    let key = key_from_hex(device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
    if lock(&shared.state).peers.get(&key).is_none() {
        return Err(failed("Runtime::NotPaired"));
    }
    // docs/audits/fable-lifecycle.md, finding 7: `save_peers`'s own shape.
    // `shared.auto_copy` is held only long enough to clone the store and,
    // once the clone has saved, to swap it back in; the clone's own
    // `write_lock` covers the `fsync` in between, so `Engine::auto_copy`
    // never waits on it.
    let write_lock = Arc::clone(&lock(&shared.auto_copy).write_lock);
    let writing = lock(&write_lock);
    let mut copy = lock(&shared.auto_copy).clone();
    copy.set_enabled(device_key_hex, enabled);
    copy.save()?;
    *lock(&shared.auto_copy) = copy;
    drop(writing);
    if enabled {
        let reachable = lock(&shared.state)
            .live
            .get(device_key_hex)
            .is_some_and(|live| live.reachable_via.is_some());
        if reachable {
            maybe_spawn_run(shared, device_key_hex);
        }
    }
    Ok(())
}

/// Called from `engine::mark_reachable` on the transition from not reachable
/// to reachable.
pub(crate) fn on_became_reachable(shared: &Arc<Shared>, device_key_hex: &str) {
    maybe_spawn_run(shared, device_key_hex);
}

/// Called once from `Engine::start`, for every device already reachable at
/// that moment.
pub(crate) fn run_for_every_reachable_enabled(shared: &Arc<Shared>) {
    let candidates: Vec<String> = {
        let state = lock(&shared.state);
        state
            .live
            .iter()
            .filter(|(_, live)| live.reachable_via.is_some())
            .map(|(key_hex, _)| key_hex.clone())
            .collect()
    };
    for key_hex in candidates {
        maybe_spawn_run(shared, &key_hex);
    }
}

/// Record that a run for this device ended, whether `run_inner` decided
/// there was nothing to queue or `transfer::finish` just watched a batch
/// this run queued reach its end.
///
/// `AutoCopy.last_run_unix_secs` and `last_run_files` are "something shown
/// beside a device", so this reports `Change::Devices` itself: a run that
/// finds nothing to queue makes no batch, and so has no other reason for
/// `transfers_changed` to fire and tell the Mac to read `auto_copy` again.
pub(crate) fn record_run(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    ended_unix_secs: i64,
    files: u32,
) {
    // docs/audits/fable-lifecycle.md, finding 7: as `set_enabled`, above.
    let write_lock = Arc::clone(&lock(&shared.auto_copy).write_lock);
    let writing = lock(&write_lock);
    let mut copy = lock(&shared.auto_copy).clone();
    copy.record_run(device_key_hex, ended_unix_secs, files);
    if copy.save().is_ok() {
        *lock(&shared.auto_copy) = copy;
    }
    drop(writing);
    // G2: a run that queued a batch holds the device's slot in
    // `auto_copy_running` past `run`'s own return, so a second reachability
    // transition while the batch is still moving files starts no second
    // run. This is where that slot is released once the batch it queued
    // reaches its end. A run that queued nothing already released its own
    // slot when `run` returned; removing an already-removed key here is a
    // no-op, so calling this unconditionally is safe either way.
    lock(&shared.auto_copy_running).remove(device_key_hex);
    notify(shared, Change::Devices);
}

// ---------------------------------------------------------------------------
// Spawning the run.
// ---------------------------------------------------------------------------

/// If this device's switch is on and no run is already in flight for it,
/// start one on a thread of its own.
fn maybe_spawn_run(shared: &Arc<Shared>, device_key_hex: &str) {
    if shared.stopping() {
        return;
    }
    let enabled = lock(&shared.auto_copy)
        .get(device_key_hex)
        .is_some_and(|row| row.enabled);
    if !enabled {
        return;
    }
    if !lock(&shared.auto_copy_running).insert(device_key_hex.to_owned()) {
        // One run per device at a time. The trigger that lost this race is
        // not lost: whatever it would have found is still there next time.
        return;
    }
    // G7: stopping may have begun in the moment since the check above.
    // Checked again here, right before spawning, so a shutdown racing this
    // call is not also handed a fresh dial to a peer it is about to tear
    // the connection down for. `keep`, below, is the authoritative close
    // of this race for whatever gap remains between this check and the
    // thread actually starting; this one just avoids the pointless work
    // when the answer is already known.
    if shared.stopping() {
        lock(&shared.auto_copy_running).remove(device_key_hex);
        return;
    }
    let shared_for_thread = Arc::clone(shared);
    let key = device_key_hex.to_owned();
    shared.keep(std::thread::spawn(move || run(&shared_for_thread, &key)));
}

/// Releases this device's slot in `shared.auto_copy_running` on every way
/// out of [`run`], including an early return or a panic, unless this run
/// queued a batch: [`RunGuard::queued`] marks that, and then the slot
/// stays held, released instead by [`record_run`] once that batch reaches
/// its end. G2: this is what stops a second reachability transition from
/// starting a second run while the first run's batch is still moving
/// files.
struct RunGuard<'a> {
    shared: &'a Arc<Shared>,
    device_key_hex: &'a str,
    queued: bool,
}

impl RunGuard<'_> {
    /// Mark that this run queued a batch, so its slot outlives `run`
    /// itself.
    fn queued(&mut self) {
        self.queued = true;
    }
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        if !self.queued {
            lock(&self.shared.auto_copy_running).remove(self.device_key_hex);
        }
    }
}

fn run(shared: &Arc<Shared>, device_key_hex: &str) {
    let mut guard = RunGuard {
        shared,
        device_key_hex,
        queued: false,
    };
    run_inner(shared, device_key_hex, &mut guard);
}

/// The run's own connection failed, or the peer's answer was too large to
/// trust. Either way the run stops here and tries again on its next
/// trigger; nothing about this is reported anywhere, the same as a `pull`
/// nobody is watching.
struct WalkFailed;

fn run_inner(shared: &Arc<Shared>, device_key_hex: &str, guard: &mut RunGuard<'_>) -> Option<()> {
    let key = key_from_hex(device_key_hex)?;
    lock(&shared.state).peers.get(&key)?;

    let (stream, socket, addr, via) = transfer::dial(shared, device_key_hex, &key).ok()?;
    mark_reachable(shared, device_key_hex, addr, via);
    let connection_id = shared.next_connection_id();
    let _socket = SocketRegistration::new(shared, connection_id, socket);
    let mut stream = StopAware::new(stream, Arc::clone(&shared.stopping));
    exchange_hello(&mut stream, &shared.display_name, shared.kind).ok()?;
    let mut client = Client::new(stream);

    let root_name = first_root_name(&mut client).ok()?;
    let Some(root_name) = root_name else {
        // docs/engine-contract.md item 14: "if the device has no roots yet,
        // the run finds nothing."
        record_run(shared, device_key_hex, now_unix_secs(), 0);
        return Some(());
    };
    lock(&shared.state)
        .live_mut(device_key_hex)
        .auto_copy_source_root = Some(root_name.clone());

    if shared.stopping() {
        return None;
    }
    let source = RemotePath::parse(&format!("{root_name}/DCIM")).ok()?;
    let files = list_recursive_with_mtime(&mut client, &source).ok()?;

    let mut to_queue: Vec<(RemotePath, u64)> = Vec::new();
    for (path, size, mtime) in files {
        if shared.stopping() {
            return None;
        }
        if lock(&shared.held).contains_path(device_key_hex, path.as_str(), size, mtime) {
            continue;
        }
        let Ok(manifest) = client.manifest(&path) else {
            // A file that could not be asked about this time may still be
            // there next run. Skipping it, not failing the whole run, is
            // what a stalled file on an otherwise healthy device deserves.
            continue;
        };
        if lock(&shared.held).contains_root(manifest.root().as_bytes()) {
            continue;
        }
        to_queue.push((path, size));
    }

    if to_queue.is_empty() {
        record_run(shared, device_key_hex, now_unix_secs(), 0);
        return Some(());
    }

    if queue_batch(shared, device_key_hex, &source, &to_queue) {
        // The slot now stays held until `record_run` releases it, once
        // `transfer::finish` sees every transfer in this batch reach `Done`
        // or `Failed`.
        guard.queued();
    }
    Some(())
}

/// Build the batch and its transfer rows the same way `Engine::pull_folder`
/// does, with `Origin::Automatic` and no caller waiting for the result:
/// `transfer::finish` records the run once every transfer in it ends. On a
/// failure to write the batch record, or to build every row, nothing is
/// queued and nothing is recorded; the files found are still new next run.
///
/// Returns whether a batch was actually queued: `run_inner` uses this to
/// decide whether its `RunGuard` keeps the device's run slot held for
/// `record_run` to release later, or releases it itself at once.
fn queue_batch(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    source: &RemotePath,
    files: &[(RemotePath, u64)],
) -> bool {
    let started_unix_secs = now_unix_secs();
    let prefix = format!("{}/", source.as_str());
    let chunk_size = *lock(&shared.chunk_size);
    let download_fs = shared.download_fs();

    let mut rows = Vec::with_capacity(files.len());
    for (full_path, _size) in files {
        let relative = full_path
            .as_str()
            .strip_prefix(&prefix)
            .unwrap_or(full_path.as_str());
        let Ok(destination) = RemotePath::parse(&format!("DCIM/{relative}")) else {
            return false;
        };
        // G5: job 7 says automatic copying never writes back. `run_inner`
        // already filtered out anything a held row names, so a file
        // already sitting at this destination was never put there by an
        // earlier run of this same file; overwriting it would destroy
        // something this run has no business touching. Land beside it
        // under a free name instead.
        let destination = match &download_fs {
            Some(fs) => free_destination(fs, destination),
            None => destination,
        };
        let Ok(session) = SessionId::generate() else {
            return false;
        };
        let id = format!("{device_key_hex}-{session}");
        let file_name = destination
            .components()
            .last()
            .unwrap_or(destination.as_str())
            .to_owned();
        rows.push(TransferRow {
            id,
            device_key_hex: device_key_hex.to_owned(),
            file_name,
            source: full_path.clone(),
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
            direction: Direction::Pull,
            speed_bytes_per_sec: None,
            batch_id: None,
            chunk_size,
        });
    }

    let Ok(batch_session) = SessionId::generate() else {
        return false;
    };
    let batch_id = format!("{device_key_hex}-{batch_session}");
    for row in &mut rows {
        row.batch_id = Some(batch_id.clone());
    }
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let batch_row = BatchRow {
        id: batch_id.clone(),
        device_key_hex: device_key_hex.to_owned(),
        label: source.as_str().to_owned(),
        direction: Direction::Pull,
        origin: Origin::Automatic,
        started_unix_secs,
        transfer_ids: ids.clone(),
        done_files: 0,
        done_bytes: 0,
    };
    if batch::write_batch(&shared.batch_path(&batch_id), &BatchRecord::of(&batch_row)).is_err() {
        return false;
    }

    {
        let mut state = lock(&shared.state);
        state.batches.insert(batch_id.clone(), batch_row);
        for row in rows {
            state.transfers.insert(row.id.clone(), row);
        }
    }
    notify(shared, Change::Transfers);
    for id in &ids {
        transfer::spawn(shared, id);
    }
    true
}

/// The most alternative names [`free_destination`] tries before giving up
/// and using the one it was asked for anyway. Never reached in practice; it
/// only bounds how long a folder somehow already holding hundreds of
/// numbered alternatives could make this loop.
const MAX_CONFLICT_ATTEMPTS: u32 = 1000;

/// `destination` if nothing is there yet in the download folder, or the
/// first `"name (2).ext"`, `"name (3).ext"`, ... alternative beside it that
/// is free.
///
/// G5: job 7 says automatic copying is one way and never writes back.
/// `run_inner` only ever reaches here with a file no held row names, so
/// anything already at `destination` was never put there by an earlier run
/// of this same file; overwriting it would destroy something this run has
/// no business touching.
fn free_destination(download_fs: &LocalFs, destination: RemotePath) -> RemotePath {
    if download_fs.stat(&destination).is_err() {
        return destination;
    }
    let leaf = leaf_of(&destination);
    let parent = destination
        .as_str()
        .strip_suffix(leaf.as_str())
        .unwrap_or("")
        .trim_end_matches('/');
    let stem = Path::new(&leaf)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(leaf.as_str());
    let extension = Path::new(&leaf)
        .extension()
        .and_then(|extension| extension.to_str());

    for n in 2..=MAX_CONFLICT_ATTEMPTS {
        let candidate_name = match extension {
            Some(extension) => format!("{stem} ({n}).{extension}"),
            None => format!("{stem} ({n})"),
        };
        let candidate_path = if parent.is_empty() {
            candidate_name
        } else {
            format!("{parent}/{candidate_name}")
        };
        if let Ok(candidate) = RemotePath::parse(&candidate_path)
            && download_fs.stat(&candidate).is_err()
        {
            return candidate;
        }
    }
    destination
}

// ---------------------------------------------------------------------------
// Listing the peer, with the same bounds `folder.rs` gives `pull_folder`.
//
// `folder::list_recursive` throws away each entry's modified time, which
// `pull_folder` never needed and this run does, for the cheap by-path skip
// check. So this walks the connection again here, over `Client` directly
// rather than through the `FileOps` adapter `folder::RemoteLister` gives
// it, reusing only the bounds and the paging rule.
// ---------------------------------------------------------------------------

fn first_root_name<S: Read + Write>(client: &mut Client<S>) -> Result<Option<String>, WalkFailed> {
    let root = RemotePath::parse("").unwrap_or_else(|_| unreachable_root());
    let mut cursor = 0u64;
    let mut pages = 0usize;
    let mut entries_seen = 0usize;
    loop {
        let (entries, next) = client.list(&root, cursor).map_err(|_| WalkFailed)?;
        pages += 1;
        entries_seen += entries.len();
        if let Some(first) = entries.into_iter().next() {
            return Ok(Some(first.name));
        }
        match folder::after_page(cursor, next, pages, entries_seen) {
            Ok(Some(next_cursor)) => cursor = next_cursor,
            Ok(None) => return Ok(None),
            Err(folder::ListTooLarge) => return Err(WalkFailed),
        }
    }
}

// `RemotePath::parse` only ever fails on the empty string for length,
// control bytes, a leading slash, or a `.`/`..` component, none of which the
// empty string can be. Kept as a function, not an `.expect`, so the reason
// is written once rather than repeated at the call site.
fn unreachable_root() -> RemotePath {
    unreachable!("the empty path always names the root")
}

fn list_recursive_with_mtime<S: Read + Write>(
    client: &mut Client<S>,
    root: &RemotePath,
) -> Result<Vec<(RemotePath, u64, i64)>, WalkFailed> {
    let mut out = Vec::new();
    walk(client, root, 1, &mut out)?;
    Ok(out)
}

fn walk<S: Read + Write>(
    client: &mut Client<S>,
    dir: &RemotePath,
    depth: u32,
    out: &mut Vec<(RemotePath, u64, i64)>,
) -> Result<(), WalkFailed> {
    if depth > folder::MAX_FOLDER_DEPTH {
        return Err(WalkFailed);
    }
    let mut cursor = 0u64;
    let mut pages = 0usize;
    let mut entries_seen = 0usize;
    loop {
        let (entries, next) = client.list(dir, cursor).map_err(|_| WalkFailed)?;
        pages += 1;
        entries_seen += entries.len();
        for entry in entries {
            let child = join(dir, &entry.name)?;
            if child == *dir {
                continue;
            }
            match entry.kind {
                ferry_core::ops::FileKind::File => {
                    out.push((child, entry.size, entry.modified_unix_secs));
                    if out.len() > folder::MAX_FOLDER_FILES {
                        return Err(WalkFailed);
                    }
                }
                ferry_core::ops::FileKind::Directory => walk(client, &child, depth + 1, out)?,
            }
        }
        match folder::after_page(cursor, next, pages, entries_seen) {
            Ok(Some(next_cursor)) => cursor = next_cursor,
            Ok(None) => break,
            Err(folder::ListTooLarge) => return Err(WalkFailed),
        }
    }
    Ok(())
}

fn join(dir: &RemotePath, name: &str) -> Result<RemotePath, WalkFailed> {
    let text = if dir.is_root() {
        name.to_owned()
    } else {
        format!("{}/{name}", dir.as_str())
    };
    RemotePath::parse(&text).map_err(|_| WalkFailed)
}

#[cfg(test)]
mod tests {
    use super::{AutoCopyStore, MAX_DEVICES};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ferry-auto-copy-test-{}-{label}-{n}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).expect("a folder for the test");
        dir
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_store() {
        let store = AutoCopyStore::load(&temp_dir("missing").join("auto_copy"));
        assert!(store.get("device").is_none());
    }

    #[test]
    fn set_enabled_then_save_then_load_round_trips() {
        let path = temp_dir("round-trip").join("auto_copy");
        let mut store = AutoCopyStore::load(&path);
        store.set_enabled("device-1", true);
        store.record_run("device-1", 1_700_000_000, 7);
        store.save().expect("the store should save");

        let loaded = AutoCopyStore::load(&path);
        let row = loaded.get("device-1").expect("the row should be there");
        assert!(row.enabled);
        assert_eq!(row.last_run_unix_secs, Some(1_700_000_000));
        assert_eq!(row.last_run_files, Some(7));
    }

    #[test]
    fn record_run_keeps_the_enabled_flag() {
        let mut store = AutoCopyStore::load(&temp_dir("keep-enabled").join("auto_copy"));
        store.set_enabled("device", true);
        store.record_run("device", 1, 0);
        assert!(
            store
                .get("device")
                .expect("the row should be there")
                .enabled
        );
    }

    #[test]
    fn turning_the_switch_off_keeps_the_last_run() {
        let mut store = AutoCopyStore::load(&temp_dir("keep-last-run").join("auto_copy"));
        store.set_enabled("device", true);
        store.record_run("device", 100, 3);
        store.set_enabled("device", false);
        let row = store.get("device").expect("the row should be there");
        assert!(!row.enabled);
        assert_eq!(
            row.last_run_unix_secs,
            Some(100),
            "turning it off is not a new run"
        );
        assert_eq!(row.last_run_files, Some(3));
    }

    #[test]
    fn a_tampered_format_version_is_refused_and_loads_empty() {
        let path = temp_dir("bad-version").join("auto_copy");
        let mut store = AutoCopyStore::load(&path);
        store.set_enabled("device", true);
        store.save().expect("the store should save");

        let mut bytes = fs::read(&path).expect("the file should be there");
        bytes[0] = 99;
        fs::write(&path, &bytes).expect("the tampered bytes should write");

        let loaded = AutoCopyStore::load(&path);
        assert!(loaded.get("device").is_none());
    }

    #[test]
    fn an_over_large_stored_count_is_refused_before_it_is_reserved() {
        use ferry_core::wire::Encoder;
        let path = temp_dir("huge-count").join("auto_copy");
        let mut e = Encoder::new();
        e.u8(1); // FORMAT_VERSION
        e.u32(u32::MAX);
        fs::write(&path, e.finish()).expect("the hand-built file should write");

        let loaded = AutoCopyStore::load(&path);
        assert!(loaded.get("anything").is_none());
        assert!(MAX_DEVICES < usize::try_from(u32::MAX).unwrap_or(usize::MAX));
    }

    #[test]
    #[cfg(unix)]
    fn on_unix_the_file_is_created_with_mode_0o600() {
        // G4: `ferry-core`'s `peers.rs` creates its own file this way. This
        // file names every paired device's automatic copy setting, which
        // deserves the same privacy.
        use std::os::unix::fs::PermissionsExt;

        let path = temp_dir("permissions").join("auto_copy");
        let mut store = AutoCopyStore::load(&path);
        store.set_enabled("device", true);
        store.save().expect("the store should save");

        let mode = fs::metadata(&path)
            .expect("the file should exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
