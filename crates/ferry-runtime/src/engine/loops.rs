//! The background loops a started engine runs, and what they poll.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::access::{self};
use crate::notify::{Change, HOLD};
use crate::state::{DeviceLive, UsbForward, clear_gone_usb_forwards, lock, now_unix_secs};
use crate::{PairingCandidate, Transport};

use super::{
    ACCESS_LOG_PRUNE, ADB_POLL, FERRY_PHONE_PORT, Shared, add_candidate, last_four, notify,
};

/// Connect to our own listener so a blocked `accept` returns.
pub(crate) fn wake_the_listener(shared: &Shared) {
    let Some(addr) = lock(&shared.state).listen_addr else {
        return;
    };
    let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, addr.port()));
    // The connection is dropped at once. Its only job is to end the wait.
    drop(TcpStream::connect_timeout(&local, Duration::from_secs(2)));
}

/// Finalise idle access log entries and prune old day files, for the whole
/// session `start` runs.
///
/// One loop does both jobs, so pruning needs no periodic thread of its
/// own: it already has to wake every [`HOLD`] to give the roll-up's five
/// second idle rule somewhere to run, which is the same cadence
/// `notify.rs` reports on, because both read the same constant
/// (docs/engine-contract.md, item 13).
///
/// `docs/audits/fable-lifecycle.md`, finding 6: a prune's directory scan
/// and unlinks used to run with `shared.access_log` locked the whole time,
/// which every served operation's `record` and every bridge request's
/// `record_this` also lock, so both stalled for one scan and some unlinks
/// once an hour. The lock is now taken twice, briefly: once to read the
/// store's own folder, and once to apply the result; `access::prune_dir`
/// runs the slow part outside it.
pub(crate) fn access_log_loop(shared: &Arc<Shared>) {
    let mut next_prune = Instant::now() + ACCESS_LOG_PRUNE;
    loop {
        let changed = {
            let mut log = lock(&shared.access_log);
            log.as_mut().is_some_and(|rollup| {
                rollup.tick(now_unix_secs());
                rollup.take_changed()
            })
        };
        if changed {
            notify(shared, Change::AccessLog);
        }
        if Instant::now() >= next_prune {
            let dir = lock(&shared.access_log)
                .as_ref()
                .map(|rollup| rollup.store_dir().to_path_buf());
            if let Some(dir) = dir
                && let Ok(removed) = access::prune_dir(&dir, now_unix_secs())
                && let Some(rollup) = lock(&shared.access_log).as_mut()
            {
                rollup.forget_pruned(&removed);
            }
            next_prune = Instant::now() + ACCESS_LOG_PRUNE;
        }
        if !shared.rest(HOLD) {
            return;
        }
    }
}

/// Ask `adb` what is plugged in, every three seconds.
pub(crate) fn adb_loop(shared: &Arc<Shared>) {
    while !shared.stopping() {
        poll_adb_once(shared);
        if !shared.rest(ADB_POLL) {
            return;
        }
    }
}

/// One pass over the plugged in devices.
fn poll_adb_once(shared: &Arc<Shared>) {
    let Some(adb) = shared.adb.as_ref() else {
        return;
    };
    let Ok(serials) = adb.devices() else {
        return;
    };

    let known: Vec<UsbForward> = lock(&shared.state).forwards.clone();
    let mut current: Vec<UsbForward> = Vec::new();
    for serial in &serials {
        if let Some(existing) = known.iter().find(|f| f.serial == *serial) {
            current.push(existing.clone());
            continue;
        }
        if let Ok(local_port) = adb.forward(serial, 0, FERRY_PHONE_PORT) {
            current.push(UsbForward {
                serial: serial.clone(),
                local_port,
            });
        }
    }
    let gone: Vec<&UsbForward> = known
        .iter()
        .filter(|f| !serials.contains(&f.serial))
        .collect();
    for forward in &gone {
        drop(adb.remove_forward(&forward.serial, forward.local_port));
    }
    {
        let mut state = lock(&shared.state);
        state.forwards.clone_from(&current);
        // A device's `usb_port` names the forward it was last reached
        // through. Once that forward is gone, the cable is gone too, and
        // `available_transports` must drop `Usb` for it rather than keep
        // showing a port nothing answers on any more.
        let gone_ports: Vec<u16> = gone.iter().map(|f| f.local_port).collect();
        clear_gone_usb_forwards(&mut state.live, &gone_ports);
    }

    for forward in &current {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, forward.local_port));
        let shown = PairingCandidate {
            id: format!("usb:{}", forward.serial),
            transport: Transport::Usb,
            short_code: last_four(&forward.serial),
        };
        add_candidate(shared, &shown, addr);
    }
}

/// Every address worth trying for one device, best first.
///
/// USB comes first because a cable is the reliable path. See decision record
/// 9 and job 2.
pub(crate) fn dial_targets(shared: &Arc<Shared>, key_hex: &str) -> Vec<(SocketAddr, Transport)> {
    let state = lock(&shared.state);
    let mut targets: Vec<(SocketAddr, Transport)> = Vec::new();
    for forward in &state.forwards {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, forward.local_port));
        targets.push((addr, Transport::Usb));
    }
    if let Some(live) = state.live.get(key_hex)
        && let Some(addr) = live.last_addr
    {
        targets.push((addr, Transport::Wifi));
    }
    for addr in &state.discovered {
        targets.push((*addr, Transport::Wifi));
    }
    let mut seen: Vec<SocketAddr> = Vec::new();
    targets.retain(|(addr, _)| {
        if seen.contains(addr) {
            false
        } else {
            seen.push(*addr);
            true
        }
    });
    targets
}

/// Write down that a device is reachable, and by which path.
pub(crate) fn mark_reachable(
    shared: &Arc<Shared>,
    key_hex: &str,
    addr: SocketAddr,
    via: Transport,
) {
    let became_reachable = {
        let mut state = lock(&shared.state);
        let live: &mut DeviceLive = state.live_mut(key_hex);
        let was_reachable = live.reachable_via.is_some();
        live.reachable_via = Some(via);
        live.last_addr = Some(addr);
        live.last_seen_unix_secs = Some(now_unix_secs());
        if via == Transport::Usb {
            live.usb_port = Some(addr.port());
        } else {
            live.last_wifi_success_unix_secs = Some(now_unix_secs());
        }
        !was_reachable
    };
    if became_reachable {
        // docs/engine-contract.md item 14: the first of the run's three
        // triggers. Outside the lock just released, since this may spawn a
        // thread.
        crate::auto_copy::on_became_reachable(shared, key_hex);
    }
}
