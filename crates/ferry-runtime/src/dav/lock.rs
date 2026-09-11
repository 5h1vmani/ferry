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
    /// `path` already holds an unexpired lock. Answered as 423 Locked: a
    /// lock only means something if a second `LOCK` cannot silently steal
    /// it and hand out a token the first caller never asked to share.
    AlreadyLocked,
}

/// What [`LockTable::unlock_path`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UnlockOutcome {
    /// `path`'s lock was removed.
    Unlocked,
    /// `path` holds no unexpired lock at all. Answered as 409 Conflict,
    /// RFC 4918's code for a lock token that names nothing.
    NotLocked,
    /// `path` holds an unexpired lock, but `token` does not match it.
    /// Answered as 403 Forbidden: unlike [`NotLocked`](Self::NotLocked),
    /// something real is being refused here.
    WrongToken,
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
    /// Every expired entry is dropped before a new one is considered, so
    /// [`MAX_LOCKS`] bounds paths actually held, not paths ever locked.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::AlreadyLocked`] when `path` already holds an
    /// unexpired lock: a second `LOCK` cannot silently replace the first
    /// caller's token with one it never asked to share. Returns
    /// [`LockError::NoRandomness`] when a token cannot be generated, and
    /// [`LockError::Full`] when the table is at [`MAX_LOCKS`] and `path`
    /// would be a new entry.
    pub(crate) fn lock_path(&self, path: &str) -> Result<String, LockError> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        held.retain(|_, held| held.expires > now);
        if held.contains_key(path) {
            return Err(LockError::AlreadyLocked);
        }
        if held.len() >= MAX_LOCKS {
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

    /// Whether a write to `path` may proceed, given the client's `If`
    /// header. `docs/engine-contract.md`, item 6, I2: "a write on a locked
    /// resource without its token is 423."
    ///
    /// Checks `path` itself and every ancestor folder, closest first: a
    /// `LOCK` always asks for `Depth: infinity` (see `server::lock_verb`'s
    /// reply), so a lock on a folder also covers everything beneath it.
    /// True once neither `path` nor any ancestor holds an unexpired lock
    /// this request's `If` header does not name, where `if_header`
    /// matching a lock's token tolerates the token with or without its
    /// `opaquelocktoken:` scheme and angle brackets, by a plain substring
    /// search: a token is thirty-two random hex characters, so nothing
    /// else in an ordinary `If` header could coincidentally contain it.
    /// This is the same tolerance `xml::requested_props` uses for a
    /// `PROPFIND` body, rather than a full parse of the `If` header's own
    /// grammar.
    pub(crate) fn allows(&self, path: &str, if_header: Option<&str>) -> bool {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        held.retain(|_, held| held.expires > now);
        for candidate in ancestors(path) {
            let Some(entry) = held.get(candidate) else {
                continue;
            };
            if !if_header.is_some_and(|header| header.contains(&entry.token)) {
                return false;
            }
        }
        true
    }

    /// Removes `path`'s lock when `token` names it and it has not expired.
    /// Accepts the token with or without its `opaquelocktoken:` scheme and
    /// angle brackets, since the `Lock-Token` and `If` headers wrap it
    /// differently.
    pub(crate) fn unlock_path(&self, path: &str, token: &str) -> UnlockOutcome {
        let token = token
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_start_matches("opaquelocktoken:");
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        held.retain(|_, held| held.expires > now);
        match held.get(path) {
            Some(entry) if entry.token == token => {
                held.remove(path);
                UnlockOutcome::Unlocked
            }
            Some(_) => UnlockOutcome::WrongToken,
            None => UnlockOutcome::NotLocked,
        }
    }
}

/// `path` itself, then each ancestor folder up to (but not including) the
/// mount root, closest first: `"Camera/Sub/b.jpg"` yields
/// `["Camera/Sub/b.jpg", "Camera/Sub", "Camera"]`. Used by
/// [`LockTable::allows`], since a lock's `Depth: infinity` means it also
/// covers everything nested inside it.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |current| {
        current.rsplit_once('/').map(|(parent, _)| parent)
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{LockError, LockTable, UnlockOutcome};

    #[test]
    fn a_lock_unlocks_with_its_own_token() {
        let table = LockTable::new();
        let token = table.lock_path("DCIM/.DS_Store").unwrap();
        assert_eq!(
            table.unlock_path("DCIM/.DS_Store", &token),
            UnlockOutcome::Unlocked
        );
    }

    #[test]
    fn unlock_refuses_the_wrong_token() {
        let table = LockTable::new();
        table.lock_path("DCIM/.DS_Store").unwrap();
        assert_eq!(
            table.unlock_path("DCIM/.DS_Store", "opaquelocktoken:not-it"),
            UnlockOutcome::WrongToken,
            "a lock is held, but the token given does not name it"
        );
    }

    #[test]
    fn unlock_of_a_path_with_no_lock_at_all_is_not_locked() {
        let table = LockTable::new();
        assert_eq!(
            table.unlock_path("never-locked", "opaquelocktoken:anything"),
            UnlockOutcome::NotLocked
        );
    }

    #[test]
    fn unlock_accepts_a_bracketed_token() {
        let table = LockTable::new();
        let token = table.lock_path("a").unwrap();
        assert_eq!(
            table.unlock_path("a", &format!("<{token}>")),
            UnlockOutcome::Unlocked
        );
    }

    #[test]
    fn a_second_lock_of_an_unexpired_lock_is_refused() {
        let table = LockTable::new();
        let first = table.lock_path("a").unwrap();
        let error = table.lock_path("a").expect_err("already locked");
        assert!(matches!(error, LockError::AlreadyLocked));
        // The first token still works: nothing about the refused second
        // `LOCK` touched it.
        assert_eq!(table.unlock_path("a", &first), UnlockOutcome::Unlocked);
    }

    #[test]
    fn a_lock_may_be_taken_again_once_the_first_is_gone() {
        let table = LockTable::new();
        let first = table.lock_path("a").unwrap();
        assert_eq!(table.unlock_path("a", &first), UnlockOutcome::Unlocked);
        table.lock_path("a").expect("the path is free again");
    }

    #[test]
    fn allows_an_unlocked_path() {
        let table = LockTable::new();
        assert!(table.allows("a", None));
    }

    #[test]
    fn allows_refuses_a_locked_path_with_no_token() {
        let table = LockTable::new();
        table.lock_path("a").unwrap();
        assert!(!table.allows("a", None));
        assert!(!table.allows("a", Some("(<opaquelocktoken:not-it>)")));
    }

    #[test]
    fn allows_accepts_the_matching_token_inside_an_if_header() {
        let table = LockTable::new();
        let token = table.lock_path("a").unwrap();
        assert!(table.allows("a", Some(&format!("(<{token}>)"))));
    }

    #[test]
    fn allows_refuses_a_child_of_a_locked_folder() {
        let table = LockTable::new();
        table.lock_path("Camera").unwrap();
        assert!(
            !table.allows("Camera/Sub/b.jpg", None),
            "a lock on a folder covers everything beneath it"
        );
    }

    #[test]
    fn allows_a_child_of_a_locked_folder_with_the_folders_own_token() {
        let table = LockTable::new();
        let token = table.lock_path("Camera").unwrap();
        assert!(table.allows("Camera/Sub/b.jpg", Some(&format!("(<{token}>)"))));
    }

    #[test]
    fn a_lock_expires() {
        let table = LockTable::with_timeout(Duration::from_millis(20));
        let token = table.lock_path("a").unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(
            table.unlock_path("a", &token),
            UnlockOutcome::NotLocked,
            "an expired lock should no longer unlock"
        );
    }
}
