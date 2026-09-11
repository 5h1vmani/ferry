//! One file per verb group, each holding the functions `respond` calls.
//!
//! - `browse.rs`: `PROPFIND` and `PROPPATCH`.
//! - `read.rs`: `GET` and `HEAD`.
//! - `write.rs`: `PUT`, `MKCOL`, `DELETE`, `MOVE`, and `COPY`.
//! - `locks.rs`: `LOCK` and `UNLOCK`.
//! - `options.rs`: `OPTIONS`, and the Basic auth check.

mod browse;

pub(crate) use browse::{propfind, propfind_probe, proppatch_verb};

mod read;

pub(crate) use read::{get_file, get_probe};
