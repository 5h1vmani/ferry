//! The methods the apps call, one file per concern. `UniFFI` hashes each
//! one's module path into the bindings, so this split changes the checksum
//! in the generated Swift and Kotlin, not the shape either one sees.
//!
//! - `lifecycle.rs`: `new`, `start`, `stop`, what the engine reports about
//!   itself, and the hidden hooks the integration tests call.
//! - `network.rs`: the Wi-Fi network the app reports, and the trusted list.
//! - `pairing.rs`: the five methods that run pairing, both ways round.
//! - `transfers.rs`: pulls, pushes, batches, and reading them back.
//! - `remote_ops.rs`: the file operations a paired device answers.
//! - `mounts.rs`: a device's `WebDAV` bridge.
//! - `auto_copy.rs`: job 7's automatic copy switch.
//! - `access.rs`: the access log.

mod access;
mod auto_copy;
mod lifecycle;
mod mounts;
mod network;
mod pairing;
mod remote_ops;
mod transfers;
