//! Reading and writing the records a run leaves behind.
//!
//! Transfer records and batch records are files in the data directory.
//! This module loads them at start, removes them on forget, and turns
//! them into the rows the apps read.

use std::path::PathBuf;
use std::sync::Arc;

use ferry_core::chunk::ChunkSize;
use ferry_core::ops::FileKind;
use ferry_core::path::RemotePath;
use ferry_core::roots::{RootSpec, Roots};
use ferry_core::session::SessionId;

use crate::access::{self};
use crate::batch::{self};
use crate::errors::{failed, from_path, from_roots};
use crate::guard::RootsState;
use crate::record::{Record, read_record};
use crate::state::{BatchRow, TransferRow, lock};
use crate::transfer::BACKOFF_MIN;
use crate::{AccessEntry, Direction, Entry, EntryKind, FerryError, Root, TransferState};

use super::Shared;

/// The sum of every listed file's size, saturating rather than overflowing.
pub(crate) fn total_listed_bytes(found: &[(RemotePath, u64)]) -> u64 {
    found
        .iter()
        .fold(0u64, |total, (_, size)| total.saturating_add(*size))
}

/// Build one `TransferRow`, still without a batch id, for each file
/// `list_recursive` found under a `pull_folder` call.
///
/// Every row is built before anything touches state, so a failure part way
/// through — only `SessionId::generate` starving of randomness can cause
/// one — leaves nothing behind, matching `pull_folder`'s "creates nothing"
/// rule on error.
pub(crate) fn rows_for_folder(
    found_files: &[(RemotePath, u64)],
    device_key_hex: &str,
    leaf: &str,
    prefix: &str,
    started_unix_secs: i64,
    chunk_size: ChunkSize,
) -> Result<Vec<TransferRow>, FerryError> {
    let mut rows = Vec::with_capacity(found_files.len());
    for (full_path, _size) in found_files {
        let relative = full_path
            .as_str()
            .strip_prefix(prefix)
            .unwrap_or(full_path.as_str());
        let destination = RemotePath::parse(&format!("{leaf}/{relative}")).map_err(from_path)?;
        let session = SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        let id = format!("{device_key_hex}-{session}");
        let file_name = leaf_of(&destination);
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
            // Filled in by the caller, once the batch id exists.
            batch_id: None,
            chunk_size,
        });
    }
    Ok(rows)
}

/// One access log entry, from the store's own type to the boundary's.
pub(crate) fn access_entry_from_core(entry: access::Entry) -> AccessEntry {
    AccessEntry {
        id: entry.id,
        device_key_hex: entry.device_key_hex,
        actor: entry.actor.into(),
        verb: entry.verb.into(),
        path: entry.path,
        bytes: entry.bytes,
        entries: entry.entries,
        files: entry.files,
        at_unix_secs: entry.at_unix_secs,
    }
}

/// Build and validate the roots a `Config` or `set_roots` call names.
///
/// # Errors
///
/// Returns a `RootsError` code, forwarded from [`Roots::open`], for
/// anything wrong with `roots` itself: no roots, an invalid or duplicate
/// name, a path that is not an existing folder, or two roots that overlap.
/// An empty `roots` from `Config` is caught before this runs, and reported
/// as `Runtime::BadConfig` instead; see [`Engine::new`].
pub(crate) fn open_roots(roots: &[Root]) -> Result<RootsState, FerryError> {
    let specs: Vec<RootSpec> = roots.iter().cloned().map(RootSpec::from).collect();
    let opened = Roots::open(specs).map_err(from_roots)?;
    Ok(RootsState {
        specs: roots.to_vec(),
        opened: Arc::new(opened),
    })
}

/// The last component of a path, for display.
///
/// `pub(crate)`: `push.rs` reuses this for a push's own `file_name`.
pub(crate) fn leaf_of(path: &RemotePath) -> String {
    path.components().last().unwrap_or(path.as_str()).to_owned()
}

/// Turn one core entry into the shape the app receives.
pub(crate) fn entry_from_core(entry: ferry_core::ops::Entry) -> Entry {
    Entry {
        name: entry.name,
        kind: match entry.kind {
            FileKind::File => EntryKind::File,
            FileKind::Directory => EntryKind::Directory,
        },
        size: entry.size,
        modified_unix_secs: entry.modified_unix_secs,
    }
}

/// Delete one transfer's record, and say so when it cannot be done.
///
/// A record that stays behind is not a small thing. Every start reads the
/// records folder, so the transfer would come back from the dead.
///
/// # Errors
///
/// Returns `TransferError::Local` when the file is there and will not go.
pub(crate) fn remove_record(shared: &Arc<Shared>, id: &str) -> Result<(), FerryError> {
    match std::fs::remove_file(shared.record_path(id)) {
        Ok(()) => Ok(()),
        // A record that is already gone is the outcome asked for.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(failed("TransferError::Local")),
    }
}

/// Delete one batch's record, and say so when it cannot be done.
///
/// # Errors
///
/// Returns `TransferError::Local` when the file is there and will not go.
pub(crate) fn remove_batch(shared: &Arc<Shared>, id: &str) -> Result<(), FerryError> {
    match std::fs::remove_file(shared.batch_path(id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(failed("TransferError::Local")),
    }
}

/// Read every transfer record left by an earlier run, as paused.
///
/// This is what makes a transfer survive the app closing. See job 3. A
/// record written during a first pass comes back with the bytes that pass
/// had already verified, so the file is not fetched again from the start.
pub(crate) fn load_saved_transfers(shared: &Arc<Shared>) {
    let Ok(entries) = std::fs::read_dir(&shared.transfers_dir) else {
        return;
    };
    let mut state = lock(&shared.state);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let Some(key_hex) = id.split_once('-').map(|(key, _)| key.to_owned()) else {
            continue;
        };
        let Ok(Some(record)) = read_record(&path) else {
            continue;
        };
        state
            .transfers
            .insert(id.clone(), row_from_record(id, key_hex, &record));
    }
}

/// The row one stored record becomes.
pub(crate) fn row_from_record(id: String, key_hex: String, record: &Record) -> TransferRow {
    let (source, destination, bytes_total, bytes_done, source_size, source_mtime, meta, chunk_size) =
        match record {
            Record::FirstPass(meta, pass) => (
                pass.source.clone(),
                pass.destination.clone(),
                pass.source_size,
                pass.bytes_done(),
                Some(pass.source_size),
                Some(pass.source_mtime),
                meta,
                // `pass.chunk_size` was a valid `ChunkSize` when this record
                // was written, since nothing else ever produces one. The
                // fallback here is never reached in practice; it exists so
                // a damaged record cannot panic this load.
                ChunkSize::new(pass.chunk_size).unwrap_or_else(|_| ChunkSize::one_mebibyte()),
            ),
            // A ready record's bytes are counted again by the resume itself,
            // which reads the partial file back and hashes it. Its manifest
            // carries the exact chunk size it was built with: `set_chunk_size`
            // after this transfer's first pass ran must not change what
            // `chunks_total` reports for it, so this comes from the record,
            // never from the engine's current setting.
            Record::Ready(meta, transfer) => (
                transfer.source.clone(),
                transfer.destination.clone(),
                transfer.manifest.length(),
                0,
                None,
                None,
                meta,
                transfer.manifest.chunk_size(),
            ),
        };
    TransferRow {
        id,
        device_key_hex: key_hex,
        file_name: leaf_of(&destination),
        source,
        destination,
        bytes_total,
        bytes_done,
        state: TransferState::Paused,
        transport: None,
        error: None,
        source_size,
        source_mtime,
        running: false,
        attempt_after: None,
        backoff: BACKOFF_MIN,
        started_unix_secs: meta.started_unix_secs,
        // A row loaded from disk is about to be requeued by `resume_all`,
        // the same as an explicit `retry`, so its end time is cleared the
        // same way. A record this crate writes never carries `Some` here
        // regardless; see `Meta::ended_unix_secs`.
        ended_unix_secs: None,
        direction: meta.direction,
        speed_bytes_per_sec: None,
        batch_id: meta.batch_id.clone(),
        chunk_size,
    }
}

/// Read every batch record left by an earlier run.
///
/// Runs after `load_saved_transfers`, so a batch's transfer ids can be
/// checked against what actually survived. A batch none of whose transfer
/// ids name a surviving row is dropped, and its file removed: the same rule
/// a single finished transfer already follows on its own, since its record
/// is deleted the moment it becomes `Done`. A batch file that does not
/// decode at all is removed the same way, rather than left on disk for
/// every future start to skip again. See `load_saved_transfers`.
pub(crate) fn load_saved_batches(shared: &Arc<Shared>) {
    let Ok(entries) = std::fs::read_dir(&shared.batches_dir) else {
        return;
    };
    let mut to_remove: Vec<PathBuf> = Vec::new();
    {
        let mut state = lock(&shared.state);
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            let Some(key_hex) = id.split_once('-').map(|(key, _)| key.to_owned()) else {
                continue;
            };
            let Some(record) = batch::read_batch(&path) else {
                to_remove.push(path);
                continue;
            };
            let survives = record
                .transfer_ids
                .iter()
                .any(|transfer_id| state.transfers.contains_key(transfer_id));
            if survives {
                state.batches.insert(
                    id.clone(),
                    BatchRow {
                        id,
                        device_key_hex: key_hex,
                        label: record.label,
                        direction: record.direction,
                        origin: record.origin,
                        started_unix_secs: record.started_unix_secs,
                        transfer_ids: record.transfer_ids,
                        done_files: record.done_files,
                        done_bytes: record.done_bytes,
                    },
                );
            } else {
                to_remove.push(path);
            }
        }
    }
    for path in to_remove {
        drop(std::fs::remove_file(path));
    }
}
