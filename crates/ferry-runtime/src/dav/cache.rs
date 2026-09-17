//! The two second depth 1 directory listing cache.
//!
//! `docs/engine-contract.md`, item 6: "a depth 1 listing is cached for two
//! seconds per folder, and dropped by any write through the bridge to that
//! folder." Only the peer's `list` result for a folder is cached here, by
//! its DAV path; a `PROPFIND` still stats the folder itself fresh on every
//! request, since that costs the peer one cheap `Stat` and the test this
//! item's cache exists for only counts `list` entries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferry_core::ops::Entry;

/// How long a cached listing stays fresh, unless a test changes it through
/// `Engine::set_list_cache_ttl`.
pub(crate) const TTL: Duration = Duration::from_secs(2);

/// How many folders this cache holds at once. A depth 1 listing entry is
/// small, but nothing here bounds how many distinct folders Finder can ask
/// for, so this stops the map growing without limit while it is still
/// only a two second cache: past this, the whole cache is cleared rather
/// than tracked entry by entry, which costs at most one extra listing per
/// folder from the peer.
const MAX_ENTRIES: usize = 1_024;

pub(crate) struct Cache {
    entries: Mutex<HashMap<String, (Vec<Entry>, Instant)>>,
    /// Shared with the engine, so `Engine::set_list_cache_ttl` reaches every
    /// bridge, whenever it was started.
    ttl: Arc<Mutex<Duration>>,
}

impl Cache {
    pub(crate) fn new(ttl: Arc<Mutex<Duration>>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    fn ttl(&self) -> Duration {
        *self
            .ttl
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The cached children of `path`, if a listing was cached within the
    /// last two seconds. `None` on a miss or an expired entry; a caller
    /// that gets `None` re-lists and calls [`Cache::put`].
    pub(crate) fn get(&self, path: &str) -> Option<Vec<Entry>> {
        let ttl = self.ttl();
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (children, at) = entries.get(path)?;
        (at.elapsed() < ttl).then(|| children.clone())
    }

    pub(crate) fn put(&self, path: &str, children: Vec<Entry>) {
        let ttl = self.ttl();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, (_, at)| at.elapsed() < ttl);
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(path) {
            entries.clear();
        }
        entries.insert(path.to_owned(), (children, Instant::now()));
    }

    /// Drops any cached listing for `path`. Called after a write through
    /// the bridge lands in that folder.
    pub(crate) fn invalidate(&self, path: &str) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{Cache, TTL};
    use ferry_core::ops::{Entry, FileKind};
    use std::sync::{Arc, Mutex};

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            kind: FileKind::File,
            size: 0,
            modified_unix_secs: 0,
        }
    }

    #[test]
    fn a_fresh_put_is_read_back() {
        let cache = Cache::new(Arc::new(Mutex::new(TTL)));
        cache.put("DCIM", vec![entry("a.jpg")]);
        assert_eq!(cache.get("DCIM").map(|e| e.len()), Some(1));
    }

    #[test]
    fn a_miss_on_another_path_returns_none() {
        let cache = Cache::new(Arc::new(Mutex::new(TTL)));
        cache.put("DCIM", vec![entry("a.jpg")]);
        assert!(cache.get("Download").is_none());
    }

    #[test]
    fn invalidate_clears_the_entry() {
        let cache = Cache::new(Arc::new(Mutex::new(TTL)));
        cache.put("DCIM", vec![entry("a.jpg")]);
        cache.invalidate("DCIM");
        assert!(cache.get("DCIM").is_none());
    }
}
