//! The prefetch thread: it reads the head of each file a listing named,
//! so the first `GET` of a file Finder is about to open is already here.

use crate::dav::errors::map_rpc_error;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferry_core::path::RemotePath;

use crate::access::AccessVerb;
use crate::engine::{Shared, record_this};
use crate::pool::{self};

use crate::dav::heads::{self};
use crate::dav::server::Bridge;

/// Reads the heads of one listing's images into the head cache, one
/// listing at a time, until `running` clears.
///
/// `docs/engine-contract.md`, item 17. One of these runs per bridge,
/// started and joined by `MountRegistry`.
pub(crate) fn prefetch_loop(shared: &Arc<Shared>, bridge: &Arc<Bridge>, running: &Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        let Some(job) = bridge.prefetch.next() else {
            return;
        };
        prefetch_job(shared, bridge, running, &job);
    }
}

/// Reads the head of every image in one listing that is not cached
/// already, and leaves one `Read` entry in this side's access log for the
/// whole listing.
///
/// Each file is read under its own pool borrow, so the prefetch holds at
/// most one of the bridge's four connections and Finder's own requests
/// interleave with it. `running` is read between files, and again on every
/// pass of [`read_head_bounded`], so `MountRegistry::stop` waits for one
/// read and not for a whole folder.
pub(crate) fn prefetch_job(
    shared: &Arc<Shared>,
    bridge: &Arc<Bridge>,
    running: &Arc<AtomicBool>,
    job: &heads::Job,
) {
    let mut files = 0u32;
    let mut bytes = 0u64;
    for file in &job.files {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if !heads::is_image(&file.name) {
            continue;
        }
        if bridge
            .heads
            .holds(&file.path, file.size, file.modified_unix_secs)
        {
            continue;
        }
        let want = file.size.min(heads::HEAD_LEN);
        if want == 0 {
            continue;
        }
        let Ok(path) = RemotePath::parse(&file.path) else {
            continue;
        };
        let Ok(mut borrowed) = bridge.pool.take(shared) else {
            // The device is not reachable. Nothing else in this listing
            // will read either, so the rest of it is dropped rather than
            // tried file by file.
            break;
        };
        let head = read_head(&mut borrowed, &path, want, running);
        drop(borrowed);
        // A head shorter than the listing said means the file changed
        // under the prefetch. Its new size and time are its own key, so
        // there is nothing here worth storing.
        let Some(head) = head.filter(|head| u64::try_from(head.len()) == Ok(want)) else {
            continue;
        };
        bytes = bytes.saturating_add(want);
        files = files.saturating_add(1);
        bridge
            .heads
            .put(&file.path, file.size, file.modified_unix_secs, head);
    }
    // Item 17: the prefetch of one listing is one `Read` entry on this
    // side, naming the folder, the bytes read, and the file count. A
    // listing that read nothing leaves no entry, since there is nothing
    // true to say about the wire.
    if files > 0 {
        record_this(
            shared,
            &bridge.device_key_hex,
            AccessVerb::Read,
            &job.folder,
            Some(bytes),
            None,
            Some(files),
        );
    }
}

/// Reads the first `want` bytes of `path` from the peer. `None` when the
/// peer's read fails, which marks the borrowed connection unhealthy the
/// same way [`stream_body`] does, and `None` when `running` clears while
/// the read is in flight.
pub(crate) fn read_head(
    borrowed: &mut pool::Borrowed<'_>,
    path: &RemotePath,
    want: u64,
    running: &AtomicBool,
) -> Option<Vec<u8>> {
    read_head_bounded(want, running, |offset, ask| {
        match borrowed.client().read(path, offset, ask) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                let (_, unhealthy) = map_rpc_error(&error);
                if unhealthy {
                    borrowed.mark_unhealthy();
                }
                None
            }
        }
    })
}

/// The read loop [`read_head`] runs, with the peer behind a closure so a
/// test can answer it without a socket.
///
/// `docs/engine-contract.md`, item 17. At most
/// [`ferry_core::limits::MAX_READS_PER_CHUNK`] reads, so a peer that
/// answers one byte at a time cannot hold this loop for as many round
/// trips as the head has bytes. `running` is read on every pass, so
/// `MountRegistry::stop` waits for one read and not for a whole head.
///
/// A head shorter than `want` is still returned. Its caller keeps only a
/// head of exactly the length the listing promised, so a short one is
/// dropped there rather than cached as if it were whole.
pub(crate) fn read_head_bounded(
    want: u64,
    running: &AtomicBool,
    mut read: impl FnMut(u64, u32) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut head: Vec<u8> = Vec::new();
    for _ in 0..ferry_core::limits::MAX_READS_PER_CHUNK {
        if !running.load(Ordering::SeqCst) {
            return None;
        }
        let read_so_far = u64::try_from(head.len()).unwrap_or(want);
        if read_so_far >= want {
            break;
        }
        let ask = u32::try_from(want - read_so_far)
            .unwrap_or(ferry_core::limits::MAX_READ_LEN)
            .min(ferry_core::limits::MAX_READ_LEN);
        let bytes = read(read_so_far, ask)?;
        if bytes.is_empty() {
            break;
        }
        head.extend_from_slice(&bytes);
    }
    Some(head)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use ferry_core::limits::MAX_READS_PER_CHUNK;

    use super::read_head_bounded;

    /// `docs/engine-contract.md`, item 17, and the third-run engine audit,
    /// finding 3. A peer that answers one byte per read would otherwise
    /// make the loop run one round trip per byte of the head: 65536 of
    /// them for one 64 KiB head.
    #[test]
    fn a_peer_that_answers_one_byte_at_a_time_is_bounded() {
        let running = AtomicBool::new(true);
        let mut calls = 0u32;
        let head = read_head_bounded(64 * 1024, &running, |_offset, _ask| {
            calls += 1;
            Some(vec![0u8; 1])
        })
        .expect("a peer that answers every read is not the failure path");
        assert_eq!(
            calls, MAX_READS_PER_CHUNK,
            "the loop must stop after MAX_READS_PER_CHUNK reads"
        );
        assert_eq!(
            head.len(),
            MAX_READS_PER_CHUNK as usize,
            "one byte per read, so the short head holds one byte per read made"
        );
    }

    /// The same finding's second half. `MountRegistry::stop` clears
    /// `running`, and the loop must read it on every pass rather than only
    /// between whole files.
    #[test]
    fn a_cleared_running_flag_ends_the_read_loop() {
        let running = AtomicBool::new(true);
        let mut calls = 0u32;
        let head = read_head_bounded(64 * 1024, &running, |_offset, _ask| {
            calls += 1;
            running.store(false, Ordering::SeqCst);
            Some(vec![0u8; 1])
        });
        assert!(
            head.is_none(),
            "a head the bridge stopped collecting is not a head to cache"
        );
        assert_eq!(calls, 1, "the flag is read before every read, not once");
    }
}
