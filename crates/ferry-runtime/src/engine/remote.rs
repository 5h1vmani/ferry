//! The one call every remote file operation goes through.

use std::sync::Arc;

use ferry_core::path::RemotePath;

use crate::FerryError;
use crate::errors::{failed, from_path};
use crate::pool::Pool;
use crate::state::{key_from_hex, lock};

use super::Shared;

/// The checks every item 19 call makes before it touches the wire.
///
/// `docs/engine-contract.md`, item 19. Parses the path, then proves the
/// engine has started and the device is paired, and hands back the parsed
/// path with the device's pool for the caller to borrow from. Every one of
/// `list`, `stat`, `read_at`, `write_at`, `truncate`, `mkdir`, `delete`
/// and `rename` opens with this, so they refuse the same things in the
/// same order.
///
/// # Errors
///
/// Returns a `PathError` code when the path is refused,
/// `Runtime::NotStarted` before `Engine::start` has run, and
/// `Runtime::NotPaired` when the key hex does not decode or names no
/// stored device. The paired check and the pool are taken together under
/// the pools lock, so a device forgotten in between is never dialed.
pub(crate) fn remote_call(
    shared: &Arc<Shared>,
    device_key_hex: &str,
    remote_path: &str,
) -> Result<(RemotePath, Arc<Pool>), FerryError> {
    let path = RemotePath::parse(remote_path).map_err(from_path)?;
    if key_from_hex(device_key_hex).is_none() {
        return Err(failed("Runtime::NotPaired"));
    }
    if !lock(&shared.state).started {
        return Err(failed("Runtime::NotStarted"));
    }
    // The paired check and the pool both live in `pool_for`, under one hold
    // of the pools lock. Checking here and making the pool afterwards let a
    // call that passed the check before `forget` wrote the peer list make a
    // fresh pool for the device it had just forgotten, and dial it.
    let pool = shared.pool_for(device_key_hex)?;
    Ok((path, pool))
}
