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

use crate::FerryError;

use super::random_hex;

/// How long a lock lasts before it is treated as gone, absent an
/// `UNLOCK`. Matches the timeout the order 0 spike found macOS accepts
/// (`Second-3600` in the probe's `LOCK` reply).
const LOCK_TIMEOUT: Duration = Duration::from_secs(3600);

struct Held {
    token: String,
    expires: Instant,
}

pub(crate) struct LockTable {
    held: Mutex<HashMap<String, Held>>,
}

impl LockTable {
    pub(crate) fn new() -> Self {
        Self {
            held: Mutex::new(HashMap::new()),
        }
    }

    /// Locks `path` and returns its token, as an `opaquelocktoken:` URI.
    ///
    /// A second `LOCK` of an already locked path replaces the old token
    /// rather than refusing: I1 checks no token before any write, so there
    /// is nothing yet for two overlapping locks to protect, and refusing
    /// the second would only make Finder retry.
    ///
    /// # Errors
    ///
    /// Returns `Runtime::MountFailed` when a token cannot be generated.
    pub(crate) fn lock_path(&self, path: &str) -> Result<String, FerryError> {
        let token = random_hex(16)?;
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(
            path.to_owned(),
            Held {
                token: token.clone(),
                expires: Instant::now() + LOCK_TIMEOUT,
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
}
