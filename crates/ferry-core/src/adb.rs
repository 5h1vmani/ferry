//! The USB transport, through an adb tunnel.
//!
//! Not implemented yet. The contract below is the specification.
//!
//! # Contract
//!
//! Decision record 9. `adb forward` opens a local TCP port on the Mac that
//! reaches a port on the phone. Everything above the socket is the existing
//! TCP transport, unchanged. This module only finds `adb`, lists devices, and
//! manages forwards.
//!
//! It shells out to the `adb` binary. It never bundles one in version 1.
//!
//! Public shape:
//!
//! ```text
//! /// Look on PATH, then in the default Android SDK location.
//! pub fn find_adb() -> Option<PathBuf>;
//!
//! pub struct Adb { .. }
//! impl Adb {
//!     pub fn new(binary: PathBuf) -> Self;
//!     /// Serial numbers of devices in the "device" state, not "unauthorized".
//!     pub fn devices(&self) -> Result<Vec<String>, AdbError>;
//!     /// Map a local port to a port on the phone. Returns the local port.
//!     pub fn forward(&self, serial: &str, local: u16, remote: u16) -> Result<u16, AdbError>;
//!     pub fn remove_forward(&self, serial: &str, local: u16) -> Result<(), AdbError>;
//! }
//! ```
//!
//! Tests do not need a phone. They point `Adb::new` at a small shell script
//! written into a temporary directory that prints what `adb` would print.
//! One test covers a device in the `unauthorized` state, which must be
//! excluded, because that is the state a phone is in before the person taps
//! "allow" on it.
