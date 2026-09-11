//! The head cache, and the queue the prefetch thread reads from.
//!
//! `docs/engine-contract.md`, item 17: Finder opens a folder of photos and
//! asks for the first bytes of every file to draw its thumbnails. Without
//! this, each of those asks costs one `Stat` and one `Read` on the wire.
//! With it, a listing is followed by the bridge reading the heads itself,
//! and a thumbnail request then costs nothing on the wire.
//!
//! # The cache
//!
//! [`HeadCache`] holds the first [`HEAD_LEN`] bytes of a file, keyed by the
//! DAV target together with the size and modified time the listing
//! reported. A saved or replaced file has a new size or time, so it misses
//! on its own and nothing has to invalidate it by hand. The cache holds at
//! most [`HEAD_CACHE_BYTES`], first in first out. It lives on `Bridge`
//! beside the listing cache, so it dies with the mount.
//!
//! # The queue
//!
//! [`Prefetch`] holds at most one listing's worth of work, of at most
//! [`PREFETCH_MAX_FILES`] targets. A new listing replaces it: Finder shows
//! one folder at a time, and the folder a person left is not worth the
//! wire. [`Prefetch::submit`] only takes a lock long enough to swap that
//! value, so a `PROPFIND` response is never held up by prefetching.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use ferry_core::ops::{Entry, FileKind};

use crate::state::lock;

/// How many bytes of a file's start the cache holds.
pub(crate) const HEAD_LEN: u64 = 64 * 1024;

/// How many bytes of heads one bridge keeps at once, across every file.
pub(crate) const HEAD_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// The most files one listing hands to the prefetch thread.
pub(crate) const PREFETCH_MAX_FILES: usize = 512;

/// The file extensions the prefetch treats as an image. Compared without
/// case. `docs/engine-contract.md`, item 17, names this list.
pub(crate) const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "heic", "heif", "gif", "webp", "tif", "tiff", "bmp", "dng", "cr2", "nef",
    "arw",
];

/// Whether `name` ends in one of [`IMAGE_EXTENSIONS`], compared without
/// case. A name with no dot in it is not an image.
pub(crate) fn is_image(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// What a cached head is filed under: the DAV target, and the size and
/// modified time the listing reported for it.
#[derive(Clone, PartialEq, Eq, Hash)]
struct HeadKey {
    target: String,
    size: u64,
    modified_unix_secs: i64,
}

struct CacheInner {
    heads: HashMap<HeadKey, Arc<[u8]>>,
    /// The keys in the order they were put, so the oldest goes first when
    /// the cache is over its byte limit.
    order: VecDeque<HeadKey>,
    bytes: usize,
}

/// The first [`HEAD_LEN`] bytes of each recently listed image.
pub(crate) struct HeadCache {
    limit_bytes: usize,
    inner: Mutex<CacheInner>,
}

impl HeadCache {
    pub(crate) fn new() -> Self {
        Self::with_limit(HEAD_CACHE_BYTES)
    }

    /// A cache with its own byte limit. `new` is the only caller outside
    /// this file's own tests, which need a limit small enough to fill.
    fn with_limit(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            inner: Mutex::new(CacheInner {
                heads: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
            }),
        }
    }

    /// The cached head of `target`, if one was stored for exactly this
    /// size and modified time.
    pub(crate) fn get(
        &self,
        target: &str,
        size: u64,
        modified_unix_secs: i64,
    ) -> Option<Arc<[u8]>> {
        let key = HeadKey {
            target: target.to_owned(),
            size,
            modified_unix_secs,
        };
        lock(&self.inner).heads.get(&key).map(Arc::clone)
    }

    /// Whether a head is already stored for exactly this size and modified
    /// time. The prefetch thread asks this before it reads anything.
    pub(crate) fn holds(&self, target: &str, size: u64, modified_unix_secs: i64) -> bool {
        self.get(target, size, modified_unix_secs).is_some()
    }

    /// Stores `head` for `target`, and drops the oldest heads until the
    /// cache is back within its byte limit. A key already stored is left
    /// as it is, so one target never counts twice.
    pub(crate) fn put(&self, target: &str, size: u64, modified_unix_secs: i64, head: Vec<u8>) {
        let key = HeadKey {
            target: target.to_owned(),
            size,
            modified_unix_secs,
        };
        let mut inner = lock(&self.inner);
        if inner.heads.contains_key(&key) {
            return;
        }
        inner.bytes = inner.bytes.saturating_add(head.len());
        inner.heads.insert(key.clone(), Arc::from(head));
        inner.order.push_back(key);
        while inner.bytes > self.limit_bytes {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some(dropped) = inner.heads.remove(&oldest) {
                inner.bytes = inner.bytes.saturating_sub(dropped.len());
            }
        }
    }
}

/// One file the prefetch may read, as the listing described it.
pub(crate) struct Target {
    /// The child's DAV target, which is also its head cache key.
    pub(crate) path: String,
    /// The child's own name, which the extension check reads.
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) modified_unix_secs: i64,
}

/// One listing's worth of work: the folder it came from, and its files.
pub(crate) struct Job {
    /// The folder's own path, which the access log entry names.
    pub(crate) folder: String,
    pub(crate) files: Vec<Target>,
}

struct QueueInner {
    /// The listing waiting to be prefetched, if any. A new listing
    /// replaces it.
    job: Option<Job>,
    /// Set by [`Prefetch::stop`], so a thread waiting for work wakes and
    /// leaves instead of waiting for a listing that will never come.
    stopped: bool,
}

/// The queue between `propfind` and one bridge's prefetch thread.
pub(crate) struct Prefetch {
    inner: Mutex<QueueInner>,
    ready: Condvar,
}

impl Prefetch {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(QueueInner {
                job: None,
                stopped: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Hands `children` to the prefetch thread as the work for `folder`,
    /// replacing whatever listing was waiting. Directories are dropped
    /// here, since a directory has no head to read, and the rest is cut to
    /// [`PREFETCH_MAX_FILES`].
    pub(crate) fn submit(&self, folder: &str, children: &[(String, Entry)]) {
        let files: Vec<Target> = children
            .iter()
            .filter(|(_, entry)| entry.kind == FileKind::File)
            .take(PREFETCH_MAX_FILES)
            .map(|(path, entry)| Target {
                path: path.clone(),
                name: entry.name.clone(),
                size: entry.size,
                modified_unix_secs: entry.modified_unix_secs,
            })
            .collect();
        if files.is_empty() {
            return;
        }
        let job = Job {
            folder: folder.to_owned(),
            files,
        };
        let mut inner = lock(&self.inner);
        if inner.stopped {
            return;
        }
        inner.job = Some(job);
        drop(inner);
        self.ready.notify_one();
    }

    /// Waits for a listing to work on. `None` once [`Prefetch::stop`] has
    /// been called, which is the prefetch thread's signal to end.
    pub(crate) fn next(&self) -> Option<Job> {
        let mut inner = lock(&self.inner);
        loop {
            if inner.stopped {
                return None;
            }
            if let Some(job) = inner.job.take() {
                return Some(job);
            }
            inner = self
                .ready
                .wait(inner)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Ends the queue. A thread waiting in [`Prefetch::next`] returns
    /// `None`, and any listing still waiting is dropped. The flag is set
    /// under the same lock the wait releases, so a stop can never be
    /// missed between the check and the wait.
    pub(crate) fn stop(&self) {
        let mut inner = lock(&self.inner);
        inner.stopped = true;
        inner.job = None;
        drop(inner);
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{HEAD_CACHE_BYTES, HEAD_LEN, HeadCache, is_image};

    #[test]
    fn every_named_extension_is_an_image_whatever_its_case() {
        for name in [
            "a.jpg", "a.jpeg", "a.png", "a.heic", "a.heif", "a.gif", "a.webp", "a.tif", "a.tiff",
            "a.bmp", "a.dng", "a.cr2", "a.nef", "a.arw",
        ] {
            assert!(is_image(name), "{name} should be an image");
            assert!(
                is_image(&name.to_uppercase()),
                "{name} should be an image in upper case too"
            );
        }
    }

    #[test]
    fn anything_else_is_not_an_image() {
        for name in ["Notes.txt", "Big.bin", "IMG_0001", "archive.jpg.zip", "jpg"] {
            assert!(!is_image(name), "{name} should not be an image");
        }
    }

    #[test]
    fn a_head_is_read_back_only_for_the_size_and_time_it_was_stored_with() {
        let cache = HeadCache::new();
        cache.put("Root/a.jpg", 100, 7, vec![1, 2, 3]);
        assert_eq!(
            cache.get("Root/a.jpg", 100, 7).as_deref(),
            Some(&[1, 2, 3][..])
        );
        assert!(
            cache.get("Root/a.jpg", 101, 7).is_none(),
            "a new size misses"
        );
        assert!(
            cache.get("Root/a.jpg", 100, 8).is_none(),
            "a new time misses"
        );
        assert!(
            cache.get("Root/b.jpg", 100, 7).is_none(),
            "another file misses"
        );
    }

    #[test]
    fn the_oldest_head_goes_first_once_the_cache_is_full() {
        // Four heads of one hundred bytes fit; the fifth pushes the first
        // one out. The same rule the thirty-two mebibyte limit runs under,
        // without allocating thirty-two mebibytes to prove it.
        let cache = HeadCache::with_limit(400);
        for i in 0..5u64 {
            cache.put(&format!("Root/{i}.jpg"), i, 0, vec![0u8; 100]);
        }
        assert!(
            cache.get("Root/0.jpg", 0, 0).is_none(),
            "the first head stored should have gone first"
        );
        for i in 1..5u64 {
            assert!(
                cache.get(&format!("Root/{i}.jpg"), i, 0).is_some(),
                "head {i} should still be held"
            );
        }
    }

    #[test]
    fn storing_the_same_head_twice_counts_it_once() {
        let cache = HeadCache::with_limit(200);
        cache.put("Root/a.jpg", 1, 0, vec![0u8; 100]);
        cache.put("Root/a.jpg", 1, 0, vec![0u8; 100]);
        cache.put("Root/b.jpg", 2, 0, vec![0u8; 100]);
        assert!(
            cache.get("Root/a.jpg", 1, 0).is_some(),
            "a repeated put must not push the cache over its limit"
        );
        assert!(cache.get("Root/b.jpg", 2, 0).is_some());
    }

    #[test]
    fn the_shipped_cache_holds_five_hundred_and_twelve_full_heads() {
        // The two constants item 17 names have to agree: thirty-two
        // mebibytes divided by a sixty-four kibibyte head is five hundred
        // and twelve files, the same bound the prefetch queue carries.
        assert_eq!(
            HEAD_CACHE_BYTES as u64 / HEAD_LEN,
            u64::try_from(super::PREFETCH_MAX_FILES).expect("a small count")
        );
    }
}
