//! Finding the phone on the local network with mDNS.
//!
//! Implemented below, following the contract in this comment.
//!
//! # Contract
//!
//! Rules from `docs/protocol.md` section 10. Only the phone advertises. The
//! instance name is random. The TXT record carries the protocol version and
//! nothing else, never a key or a fingerprint. Advertising can be stopped.
//!
//! Built on the `mdns-sd` crate, which needs no async runtime.
//!
//! Public shape:
//!
//! ```text
//! pub const SERVICE_TYPE: &str = "_ferry._tcp.local.";
//!
//! /// The phone side. Announces one service until dropped or stopped.
//! pub struct Advertiser { .. }
//! impl Advertiser {
//!     /// Announce on `port` under a fresh random instance name.
//!     pub fn start(port: u16) -> Result<Self, DiscoveryError>;
//!     pub fn instance_name(&self) -> &str;
//!     pub fn stop(self);
//! }
//!
//! /// The Mac side. Reports services as they appear and disappear.
//! pub struct Browser { .. }
//! impl Browser {
//!     pub fn start() -> Result<Self, DiscoveryError>;
//!     /// Block up to `timeout` for the next event.
//!     pub fn next(&self, timeout: Duration) -> Option<Event>;
//!     pub fn stop(self);
//! }
//!
//! pub enum Event {
//!     Found { instance: String, addr: SocketAddr, version: u16 },
//!     Lost { instance: String },
//! }
//! ```
//!
//! A test advertises and browses inside one process. It is marked
//! `#[ignore]` with a reason, because it needs a real multicast-capable
//! interface and CI runners do not reliably have one. It is run by hand.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::peers::to_hex;
use crate::version::VERSION_MAX;

/// The mDNS service type Ferry advertises and browses for.
pub const SERVICE_TYPE: &str = "_ferry._tcp.local.";

/// The reason discovery failed.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The mdns-sd daemon reported a failure.
    #[error("mdns failed: {0}")]
    Mdns(#[from] mdns_sd::Error),
    /// The system supplied no random bytes for the instance name.
    #[error("the system supplied no random bytes")]
    NoRandomness,
}

/// The phone side. Announces one service until dropped or stopped.
///
/// Dropping an `Advertiser` shuts down its mDNS daemon, which withdraws the
/// announcement. There is nothing left running once the value is gone.
pub struct Advertiser {
    daemon: ServiceDaemon,
    instance: String,
}

impl Advertiser {
    /// Announce on `port` under a fresh random instance name.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::NoRandomness`] when the system cannot supply
    /// random bytes for the instance name, and [`DiscoveryError::Mdns`] when
    /// the daemon cannot be created or the service cannot be registered.
    pub fn start(port: u16) -> Result<Self, DiscoveryError> {
        let instance = random_instance_name()?;
        let service_info = build_service_info(&instance, port)?;
        let daemon = ServiceDaemon::new()?;
        daemon.register(service_info)?;
        Ok(Self { daemon, instance })
    }

    /// The random instance name this advertiser announced under.
    #[must_use]
    pub fn instance_name(&self) -> &str {
        &self.instance
    }

    /// Stop advertising.
    ///
    /// This only consumes the value. The actual shutdown happens in `Drop`,
    /// so a plain `drop(advertiser)` does the same thing.
    pub fn stop(self) {}
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        // The daemon thread is going away regardless. A failure here has
        // nothing left to report to.
        let _ = self.daemon.shutdown();
    }
}

/// The Mac side. Reports services as they appear and disappear.
///
/// Dropping a `Browser` shuts down its mDNS daemon, which stops the search.
pub struct Browser {
    daemon: ServiceDaemon,
    receiver: mdns_sd::Receiver<ServiceEvent>,
}

impl Browser {
    /// Start browsing for [`SERVICE_TYPE`].
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::Mdns`] when the daemon cannot be created or
    /// the browse cannot be started.
    pub fn start() -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let receiver = daemon.browse(SERVICE_TYPE)?;
        Ok(Self { daemon, receiver })
    }

    /// Block up to `timeout` for the next event.
    ///
    /// Reads events from the mdns-sd daemon and keeps waiting, within the
    /// same `timeout` budget, past any event this module does not report: a
    /// resolved service with no usable address or version, and every event
    /// kind other than a resolve or a removal. Returns `None` once the
    /// budget runs out with nothing to report.
    #[must_use]
    pub fn next(&self, timeout: Duration) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let event = self.receiver.recv_timeout(remaining).ok()?;
            match event {
                ServiceEvent::ServiceResolved(resolved) => {
                    if let Some(found) = found_event(&resolved) {
                        return Some(found);
                    }
                    // No usable address or version. Keep waiting.
                }
                ServiceEvent::ServiceRemoved(_service_type, fullname) => {
                    return Some(Event::Lost {
                        instance: instance_from_fullname(&fullname),
                    });
                }
                _ => {}
            }
        }
    }

    /// Stop browsing.
    ///
    /// This only consumes the value. The actual shutdown happens in `Drop`,
    /// so a plain `drop(browser)` does the same thing.
    pub fn stop(self) {}
}

impl Drop for Browser {
    fn drop(&mut self) {
        // The daemon thread is going away regardless. A failure here has
        // nothing left to report to.
        let _ = self.daemon.shutdown();
    }
}

/// One thing that happened while browsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A service was found and resolved to an address.
    Found {
        /// The random instance name the phone announced under.
        instance: String,
        /// The address and port to connect to.
        addr: SocketAddr,
        /// The protocol version the phone advertised, parsed from its TXT
        /// record.
        version: u16,
    },
    /// A previously found service is no longer advertised.
    Lost {
        /// The random instance name that stopped being advertised.
        instance: String,
    },
}

/// Build the [`Event::Found`] for a resolved service, or `None` when the
/// service has no usable IPv4 address, no `v` property, or a `v` property
/// that does not parse as `u16`.
fn found_event(resolved: &mdns_sd::ResolvedService) -> Option<Event> {
    let ip = resolved.get_addresses_v4().into_iter().next()?;
    let version = resolved
        .get_property("v")
        .and_then(|prop| prop.val_str().parse().ok())?;
    Some(Event::Found {
        instance: instance_from_fullname(resolved.get_fullname()),
        addr: SocketAddr::new(IpAddr::V4(ip), resolved.get_port()),
        version,
    })
}

/// Recover the instance name from a full mDNS name such as
/// `"0123456789abcdef._ferry._tcp.local."`.
///
/// Falls back to the full name when it does not carry the expected suffix,
/// which should not happen for names this module itself put on the network.
fn instance_from_fullname(fullname: &str) -> String {
    fullname
        .strip_suffix(SERVICE_TYPE)
        .and_then(|rest| rest.strip_suffix('.'))
        .unwrap_or(fullname)
        .to_string()
}

/// Build the `ServiceInfo` an [`Advertiser`] registers.
///
/// The host name is `instance` plus `.local.`, never the machine's real
/// hostname, so nothing about the machine leaks. No address is given up
/// front. Address auto-detection is turned on instead, so mdns-sd fills in
/// the host's own addresses when it registers the service.
fn build_service_info(instance: &str, port: u16) -> Result<ServiceInfo, DiscoveryError> {
    let host_name = format!("{instance}.local.");
    let mut txt_properties = HashMap::with_capacity(1);
    txt_properties.insert("v".to_string(), VERSION_MAX.to_string());
    let info = ServiceInfo::new(SERVICE_TYPE, instance, &host_name, (), port, txt_properties)?
        .enable_addr_auto();
    Ok(info)
}

/// Build a fresh random instance name: 8 random bytes, hex encoded, so 16
/// hex characters. Never the machine's hostname.
fn random_instance_name() -> Result<String, DiscoveryError> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| DiscoveryError::NoRandomness)?;
    Ok(to_hex(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        Advertiser, Browser, Event, VERSION_MAX, build_service_info, random_instance_name,
    };
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    #[test]
    fn instance_names_are_random_and_16_lowercase_hex_characters() {
        let a = random_instance_name().unwrap();
        let b = random_instance_name().unwrap();
        assert_ne!(a, b, "two random names should not collide");
        for name in [&a, &b] {
            assert_eq!(name.len(), 16, "name {name} is not 16 characters");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "name {name} is not lowercase hex"
            );
        }
    }

    #[test]
    fn the_txt_record_holds_only_the_version() {
        let info = build_service_info("0123456789abcdef", 12345).unwrap();
        let properties: HashMap<String, String> = info
            .get_properties()
            .iter()
            .map(|prop| (prop.key().to_string(), prop.val_str().to_string()))
            .collect();
        let mut expected = HashMap::new();
        expected.insert("v".to_string(), VERSION_MAX.to_string());
        assert_eq!(properties, expected);
    }

    #[test]
    #[ignore = "needs a multicast-capable interface; run by hand"]
    fn a_browser_finds_an_advertised_service() {
        let port = 54237;
        let advertiser = Advertiser::start(port).expect("advertiser should start");
        let browser = Browser::start().expect("browser should start");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = None;
        while found.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Some(Event::Found {
                instance,
                addr,
                version,
            }) = browser.next(remaining)
                && instance == advertiser.instance_name()
            {
                found = Some((addr, version));
            }
        }

        let (addr, version) = found.expect("should find the advertised service within 5 seconds");
        assert_eq!(addr.port(), port);
        assert_eq!(version, VERSION_MAX);
    }
}
