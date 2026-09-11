//! The lock table `LOCK` and `UNLOCK` share.
//!
//! `docs/spike-0-findings.md`, question 4: Finder locks a file before it
//! writes `.DS_Store`, so `LOCK` and `UNLOCK` work in I1 even though every
//! other write verb answers 403. Item I2 will check a token from this
//! table against the `If` header on the verbs it adds; I1 has no such
//! verb, so this table only ever gains and loses entries here.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::random_hex;

/// How long a lock lasts before it is treated as gone, absent an
/// `UNLOCK`. Matches the timeout the order 0 spike found macOS accepts
/// (`Second-3600` in the probe's `LOCK` reply).
const LOCK_TIMEOUT: Duration = Duration::from_secs(3600);

/// How many paths this table may hold locked at once. Expired entries are
/// dropped before this is checked, so a table that looks full is usually
/// one Finder forgot to `UNLOCK`, not 4,096 files genuinely open at once.
const MAX_LOCKS: usize = 4_096;

/// Why [`LockTable::lock_path`] refused a lock.
#[derive(Debug)]
pub(crate) enum LockError {
    /// The platform's randomness source was exhausted generating a token.
    NoRandomness,
    /// The table already holds [`MAX_LOCKS`] entries, and `path` is not
    /// one of them. Answered as 507 Insufficient Storage: this is the
    /// server's own table being full, not another resource's lock in the
    /// way, which is what 423 Locked means in RFC 4918.
    Full,
}

struct Held {
    token: String,
    expires: Instant,
}

pub(crate) struct LockTable {
    held: Mutex<HashMap<String, Held>>,
    /// [`LOCK_TIMEOUT`] in production; a test builds this shorter with
    /// [`LockTable::with_timeout`] so a lock can be seen to expire without
    /// the test itself waiting an hour.
    timeout: Duration,
}

impl LockTable {
    pub(crate) fn new() -> Self {
        Self {
            held: Mutex::new(HashMap::new()),
            timeout: LOCK_TIMEOUT,
        }
    }

    /// Builds a table whose locks expire after `timeout` instead of
    /// [`LOCK_TIMEOUT`]. Test only.
    #[cfg(test)]
    fn with_timeout(timeout: Duration) -> Self {
        Self {
            held: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    /// Locks `path` and returns its token, as an `opaquelocktoken:` URI.
    ///
    /// A second `LOCK` of an already locked path replaces the old token
    /// rather than refusing: I1 checks no token before any write, so there
    /// is nothing yet for two overlapping locks to protect, and refusing
    /// the second would only make Finder retry.
    ///
    /// Every expired entry is dropped before a new one is considered, so
    /// [`MAX_LOCKS`] bounds paths actually held, not paths ever locked.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::NoRandomness`] when a token cannot be
    /// generated, and [`LockError::Full`] when the table is at
    /// [`MAX_LOCKS`] and `path` would be a new entry.
    pub(crate) fn lock_path(&self, path: &str) -> Result<String, LockError> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        held.retain(|_, held| held.expires > now);
        if !held.contains_key(path) && held.len() >= MAX_LOCKS {
            return Err(LockError::Full);
        }
        let token = random_hex(16).map_err(|_| LockError::NoRandomness)?;
        held.insert(
            path.to_owned(),
            Held {
                token: token.clone(),
                expires: now + self.timeout,
            },
        );
        Ok(format!("opaquelocktoken:{token}"))
    }

    /// Removes `path`'s lock when `token` names it and it has not expired.
    /// Returns whether it did. Accepts the token with or without its
    /// `opaquelocktoken:` scheme and angle brackets, since the `Lock-Token`
    /// and `If` headers wrap it differently.
    pub(crate) fn unlock_path(&self, path: &str, token: &str) -> bool {
        let token = token
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_start_matches("opaquelocktoken:");
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match held.get(path) {
            Some(entry) if entry.token == token && entry.expires > Instant::now() => {
                held.remove(path);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::LockTable;

    #[test]
    fn a_lock_unlocks_with_its_own_token() {
        let table = LockTable::new();
        let token = table.lock_path("DCIM/.DS_Store").unwrap();
        assert!(table.unlock_path("DCIM/.DS_Store", &token));
    }

    #[test]
    fn unlock_refuses_the_wrong_token() {
        let table = LockTable::new();
        table.lock_path("DCIM/.DS_Store").unwrap();
        assert!(!table.unlock_path("DCIM/.DS_Store", "opaquelocktoken:not-it"));
    }

    #[test]
    fn unlock_accepts_a_bracketed_token() {
        let table = LockTable::new();
        let token = table.lock_path("a").unwrap();
        assert!(table.unlock_path("a", &format!("<{token}>")));
    }

    #[test]
    fn a_second_lock_replaces_the_first() {
        let table = LockTable::new();
        let first = table.lock_path("a").unwrap();
        let second = table.lock_path("a").unwrap();
        assert_ne!(first, second);
        assert!(!table.unlock_path("a", &first));
        assert!(table.unlock_path("a", &second));
    }

    #[test]
    fn a_lock_expires() {
        let table = LockTable::with_timeout(Duration::from_millis(20));
        let token = table.lock_path("a").unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !table.unlock_path("a", &token),
            "an expired lock should no longer unlock"
        );
    }
}
