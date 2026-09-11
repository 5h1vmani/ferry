//! Queuing pulls, pushes, and the batches they belong to, and reading
//! back every transfer and device the app can show.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ferry_core::ops::OpError;
use ferry_core::path::{PathError, RemotePath};
use ferry_core::rpc::{Client, exchange_hello};
use ferry_core::session::SessionId;

use crate::access;
use crate::batch::{self, BatchRecord};
use crate::engine::{
    Engine, SocketRegistration, leaf_of, mark_reachable, notify, record_this, remove_batch,
    remove_record, rows_for_folder, save_peers, total_listed_bytes,
};
use crate::errors::{failed, failed_with, from_op, from_path, from_rpc};
use crate::folder::{self, ListRecursiveError, RemoteLister};
use crate::guard::StopAware;
use crate::notify::Change;
use crate::push;
use crate::state::{BatchRow, TransferRow, key_from_hex, lock, now_unix_secs};
use crate::transfer::{self, BACKOFF_MIN};
use crate::{BatchInfo, DeviceInfo, Direction, FerryError, Origin, TransferInfo, TransferState};

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
impl Engine {
    /// Every paired device, with what is known about it right now.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceInfo> {
        lock(&self.shared.state).devices()
    }

    /// Forget a device: remove its key and every transfer record for it.
    ///
    /// A connection that is already serving this device stops answering at
    /// once, though its socket stays open until the peer goes away.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired` when no device has that key, a
    /// `PeerError` code when the device list cannot be written, and
    /// `TransferError::Local` when a transfer record cannot be deleted. The
    /// last one matters: a record left on disk would start the transfer
    /// again on the next run.
    pub fn forget(&self, key_hex: String) -> Result<(), FerryError> {
        let key = key_from_hex(&key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        if lock(&self.shared.state).peers.get(&key).is_none() {
            return Err(failed("Runtime::NotPaired"));
        }
        // `forget` stops the device's bridge. `docs/engine-contract.md`,
        // item 6, and closes its pool, item 19. The bridge is stopped first,
        // so no request can make the pool again after it is dropped.
        //
        // Removing the registry's handle is not enough. A bridge connection
        // Finder already holds keeps its own `Arc<Bridge>`, which keeps the
        // `Arc<Pool>`, so its next request would pop an idle connection
        // nobody had closed. `Pool::close` shuts every idle connection down
        // and refuses every later borrow.
        self.shared.mounts.stop(&key_hex);
        {
            // One hold of the pools lock covers the removal, the close, and
            // the peer list write. `Shared::pool_for` takes the same lock
            // across its own paired check, so no call can pass that check
            // while this is running and then make the pool again.
            let mut pools = lock(&self.shared.pools);
            if let Some(pool) = pools.remove(&key_hex) {
                pool.close();
            }
            save_peers(&self.shared, |store| {
                drop(store.remove(&key));
                Ok(())
            })?;
            drop(pools);
        }

        let gone: Vec<String>;
        let gone_batches: Vec<String>;
        {
            let mut state = lock(&self.shared.state);
            if let Some(live) = state.live.remove(&key_hex) {
                for switch in live.serving {
                    switch.store(false, Ordering::SeqCst);
                }
            }
            gone = state
                .transfers
                .values()
                .filter(|row| row.device_key_hex == key_hex)
                .map(|row| row.id.clone())
                .collect();
            for id in &gone {
                state.transfers.remove(id);
            }
            gone_batches = state
                .batches
                .values()
                .filter(|batch| batch.device_key_hex == key_hex)
                .map(|batch| batch.id.clone())
                .collect();
            for id in &gone_batches {
                state.batches.remove(id);
            }
        }
        let mut trouble = None;
        for id in &gone {
            if let Err(error) = remove_record(&self.shared, id) {
                trouble = Some(error);
            }
        }
        for id in &gone_batches {
            if let Err(error) = remove_batch(&self.shared, id) {
                trouble = Some(error);
            }
        }
        notify(&self.shared, Change::Devices);
        notify(&self.shared, Change::Transfers);
        match trouble {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Every transfer, as the app shows them.
    #[must_use]
    pub fn transfers(&self) -> Vec<TransferInfo> {
        lock(&self.shared.state)
            .transfers
            .values()
            .map(TransferRow::info)
            .collect()
    }

    /// Every batch this engine has grouped, across every device.
    #[must_use]
    pub fn batches(&self) -> Vec<BatchInfo> {
        let state = lock(&self.shared.state);
        state
            .batches
            .values()
            .map(|batch| batch.info(&state.transfers))
            .collect()
    }

    /// Fetch one file from a paired device into the shared root.
    ///
    /// Returns the transfer identifier. The work runs on its own thread and
    /// reports through the listener.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when either path is refused,
    /// `Runtime::NotPaired` when that device is not stored, and
    /// `Runtime::NotStarted` before [`Engine::start`] has run.
    /// `PathError::Empty` is one such code: it names the shared root, which
    /// has no single file to pull.
    pub fn pull(
        &self,
        device_key_hex: String,
        remote_path: String,
        local_name: String,
    ) -> Result<String, FerryError> {
        let source = RemotePath::parse(&remote_path).map_err(from_path)?;
        if source.is_root() {
            return Err(from_path(PathError::Empty));
        }
        let destination = RemotePath::parse(&local_name).map_err(from_path)?;
        // Refused here and not only where the transfer is built, because
        // the first pass fetches the whole file before that point.
        if destination.is_root() {
            return Err(from_path(PathError::Empty));
        }
        let key = key_from_hex(&device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;

        let id = {
            let mut state = lock(&self.shared.state);
            if !state.started {
                return Err(failed("Runtime::NotStarted"));
            }
            if state.peers.get(&key).is_none() {
                return Err(failed("Runtime::NotPaired"));
            }
            let session =
                SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
            // The key is part of the identifier so that a restart can tell
            // which device a record on disk belongs to, and so `forget` can
            // find every record for one device by its name alone.
            let id = format!("{device_key_hex}-{session}");
            let file_name = leaf_of(&destination);
            state.transfers.insert(
                id.clone(),
                TransferRow {
                    id: id.clone(),
                    device_key_hex: device_key_hex.clone(),
                    file_name,
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
                    started_unix_secs: now_unix_secs(),
                    ended_unix_secs: None,
                    direction: Direction::Pull,
                    speed_bytes_per_sec: None,
                    batch_id: None,
                    chunk_size: *lock(&self.shared.chunk_size),
                },
            );
            id
        };

        notify(&self.shared, Change::Transfers);
        transfer::spawn(&self.shared, &id);
        Ok(id)
    }

    /// Copy a whole folder into one batch.
    ///
    /// Lists `remote_path` recursively over the connection, using the same
    /// paging `list` uses, then creates the batch and queues one transfer
    /// per file found, in listing order. Blocks until the listing is done,
    /// so the app calls it off the main thread, the same way it calls
    /// `list`.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when the path is refused,
    /// `Runtime::NotPaired` when the device is not stored,
    /// `Runtime::NotStarted` before [`Engine::start`] has run,
    /// `Runtime::NotReachable` when no dial succeeds, an `OpError` code when
    /// the peer refuses the folder itself, and `Runtime::FolderTooLarge` at
    /// more than 10,000 files or more than 32 levels of nesting. Nothing is
    /// queued when this returns an error.
    pub fn pull_folder(
        &self,
        device_key_hex: String,
        remote_path: String,
    ) -> Result<String, FerryError> {
        let source = RemotePath::parse(&remote_path).map_err(from_path)?;
        if source.is_root() {
            return Err(from_path(PathError::Empty));
        }
        let key = key_from_hex(&device_key_hex).ok_or_else(|| failed("Runtime::NotPaired"))?;
        {
            let state = lock(&self.shared.state);
            if !state.started {
                return Err(failed("Runtime::NotStarted"));
            }
            if state.peers.get(&key).is_none() {
                return Err(failed("Runtime::NotPaired"));
            }
        }

        let (stream, socket, addr, via) = transfer::dial(&self.shared, &device_key_hex, &key)?;
        mark_reachable(&self.shared, &device_key_hex, addr, via);
        // docs/engine-contract.md item 16c: registered for the life of this
        // call, so `stop` can close it if the listing hangs.
        let connection_id = self.shared.next_connection_id();
        let _socket = SocketRegistration::new(&self.shared, connection_id, socket);
        let mut stream = StopAware::new(stream, Arc::clone(&self.shared.stopping));
        exchange_hello(&mut stream, &self.shared.display_name, self.shared.kind)
            .map_err(|error| from_rpc(&error))?;
        let mut client = Client::new(stream);

        let lister = RemoteLister::new(&mut client);
        let found_files =
            folder::list_recursive(&lister, &source).map_err(|error| match error {
                ListRecursiveError::TooLarge => failed("Runtime::FolderTooLarge"),
                ListRecursiveError::Op(OpError::Internal) => lister
                    .take_failure()
                    .map_or_else(|| from_op(OpError::Internal), |rpc| from_rpc(&rpc)),
                ListRecursiveError::Op(op) => from_op(op),
            })?;

        let leaf = leaf_of(&source);
        let prefix = format!("{}/", source.as_str());
        let started_unix_secs = now_unix_secs();

        // docs/engine-contract.md, item 13: one Read entry for the whole
        // listing, with the file count and the byte total the listing
        // itself already reported, before a single byte of any file has
        // moved.
        record_this(
            &self.shared,
            &device_key_hex,
            access::AccessVerb::Read,
            source.as_str(),
            Some(total_listed_bytes(&found_files)),
            None,
            Some(u32::try_from(found_files.len()).unwrap_or(u32::MAX)),
        );

        let mut rows = rows_for_folder(
            &found_files,
            &device_key_hex,
            &leaf,
            &prefix,
            started_unix_secs,
            *lock(&self.shared.chunk_size),
        )?;

        let batch_session =
            SessionId::generate().map_err(|_| failed("TransferError::NoRandomness"))?;
        let batch_id = format!("{device_key_hex}-{batch_session}");
        for row in &mut rows {
            row.batch_id = Some(batch_id.clone());
        }
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let batch_row = BatchRow {
            id: batch_id.clone(),
            device_key_hex: device_key_hex.clone(),
            label: remote_path,
            direction: Direction::Pull,
            origin: Origin::Manual,
            started_unix_secs,
            transfer_ids: ids.clone(),
            done_files: 0,
            done_bytes: 0,
        };
        batch::write_batch(
            &self.shared.batch_path(&batch_id),
            &BatchRecord::of(&batch_row),
        )?;

        {
            let mut state = lock(&self.shared.state);
            state.batches.insert(batch_id.clone(), batch_row);
            for row in rows {
                state.transfers.insert(row.id.clone(), row);
            }
        }
        notify(&self.shared, Change::Transfers);
        for id in &ids {
            transfer::spawn(&self.shared, id);
        }
        Ok(batch_id)
    }

    /// Send one file to a paired device.
    ///
    /// `docs/engine-contract.md`, item 5. `local_path` is absolute on this
    /// device; `remote_path` is root-relative on the peer and names the
    /// file, not its folder. Runs on its own thread, the same as `pull`, and
    /// resumes on its own when the device becomes reachable again.
    ///
    /// # Errors
    ///
    /// Returns a `PathError` code when `remote_path` is refused, and
    /// `Runtime::NotPaired` when the device is not stored. A local file that
    /// is missing, a directory, a symlink, or a special file, and a
    /// read-only root on the peer, surface as the matching error on the
    /// transfer row instead, once a worker attempts it. See `push.rs`.
    pub fn push(
        &self,
        device_key_hex: String,
        local_path: String,
        remote_path: String,
    ) -> Result<String, FerryError> {
        push::push(&self.shared, &device_key_hex, &local_path, &remote_path)
    }

    /// Send several files into one folder on a paired device, as one batch.
    ///
    /// `docs/engine-contract.md`, item 5. Each file lands at
    /// `remote_folder/<file name>`. Dials the device to confirm
    /// `remote_folder` is really a folder before anything is queued, the
    /// same way `pull_folder` confirms its own folder by listing it.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::NotPaired`, `Runtime::NotStarted`, an `OpError`
    /// code when the dial or the folder check fails, and
    /// `OpError::NotADirectory` when `remote_folder` names a file on the
    /// peer.
    pub fn push_files(
        &self,
        device_key_hex: String,
        local_paths: Vec<String>,
        remote_folder: String,
    ) -> Result<String, FerryError> {
        push::push_files(&self.shared, &device_key_hex, &local_paths, &remote_folder)
    }

    /// Restart a failed transfer from its resume point.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::TransferNotFound` when no transfer has that
    /// identifier, and `Runtime::NotPaired` when its device has been
    /// forgotten. In the second case the transfer and its record are dropped
    /// before the error is returned.
    pub fn retry(&self, transfer_id: String) -> Result<(), FerryError> {
        {
            let mut state = lock(&self.shared.state);
            let row = state
                .transfers
                .get(&transfer_id)
                .ok_or_else(|| failed("Runtime::TransferNotFound"))?;
            // A transfer with no device behind it has nowhere to go, and a
            // record left on disk is what brings a forgotten device back.
            let paired = key_from_hex(&row.device_key_hex)
                .is_some_and(|key| state.peers.get(&key).is_some());
            if !paired {
                state.transfers.remove(&transfer_id);
                drop(state);
                drop(remove_record(&self.shared, &transfer_id));
                notify(&self.shared, Change::Transfers);
                return Err(failed("Runtime::NotPaired"));
            }
            let row = state
                .transfers
                .get_mut(&transfer_id)
                .ok_or_else(|| failed("Runtime::TransferNotFound"))?;
            // A finished transfer has no record and no partial file left, so
            // a retry would fetch the whole file again. That is a new pull,
            // not a retry.
            if row.running || row.state == TransferState::Done {
                return Ok(());
            }
            row.state = TransferState::Queued;
            row.error = None;
            row.ended_unix_secs = None;
        }
        notify(&self.shared, Change::Transfers);
        transfer::spawn(&self.shared, &transfer_id);
        Ok(())
    }

    /// Retry every `Failed` transfer in a batch.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::TransferNotFound`, with the batch id as detail,
    /// when no batch has that identifier. Otherwise behaves as calling
    /// [`Engine::retry`] on each of the batch's `Failed` transfers in turn:
    /// each one that is not paired any more is dropped rather than retried,
    /// and that device's `Runtime::NotPaired` is not itself an error here.
    pub fn retry_batch(&self, batch_id: String) -> Result<(), FerryError> {
        let ids: Vec<String> = {
            let state = lock(&self.shared.state);
            let batch = state
                .batches
                .get(&batch_id)
                .ok_or_else(|| failed_with("Runtime::TransferNotFound", &batch_id))?;
            batch
                .transfer_ids
                .iter()
                .filter(|id| {
                    state
                        .transfers
                        .get(id.as_str())
                        .is_some_and(|row| row.state == TransferState::Failed)
                })
                .cloned()
                .collect()
        };
        for id in ids {
            match self.retry(id) {
                Ok(()) => {}
                // `retry` itself already dropped the transfer and its
                // record; the whole batch retry is not a failure because
                // one device in it was forgotten mid-retry.
                Err(FerryError::Failed { code, .. }) if code == "Runtime::NotPaired" => {}
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }
}
