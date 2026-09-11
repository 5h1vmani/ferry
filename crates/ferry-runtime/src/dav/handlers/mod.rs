//! One file per verb group, each holding the functions `respond` calls.
//!
//! - `browse.rs`: `PROPFIND` and `PROPPATCH`.
//! - `read.rs`: `GET` and `HEAD`.
//! - `write.rs`: `PUT`, `MKCOL`, `DELETE`, `MOVE`, and `COPY`.
//! - `locks.rs`: `LOCK` and `UNLOCK`.
//! - `options.rs`: `OPTIONS`, and the Basic auth check.
//!
//! A probe is answered from the sidecar store, never from the peer.

mod browse;

pub(crate) use browse::{propfind, propfind_probe, proppatch_verb};

mod read;

pub(crate) use read::{get_file, get_probe};

mod write;

pub(crate) use write::{
    copy_verb, delete_verb, map_write_error, mkcol_verb, move_verb, put_file, put_sidecar,
};

mod locks;

pub(crate) use locks::{lock_verb, unlock_verb};

mod options;

pub(crate) use options::{ALLOWED_METHODS, authorized, options};
