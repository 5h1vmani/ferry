//! The one road every callback takes to the app.
//!
//! Two rules live here. First, nothing may reach the app after
//! [`Engine::stop`](crate::Engine::stop) has returned, so the listener sits
//! in a slot that `stop` empties. A notification that finds an empty slot
//! does nothing. Second, a busy engine must not call the app hundreds of
//! times a second, so a change waits a short while and is reported once,
//! however many times it happened.
//!
//! A pairing state is different. Each one carries a value the screen needs,
//! and they are rare, so they are reported as they happen.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::state::lock;
use crate::{EngineListener, PairingState};

/// How long a device or transfer change waits before it is reported.
const HOLD: Duration = Duration::from_millis(250);

/// How long a new pairing candidate waits before the list is reported.
///
/// Longer than [`HOLD`], because the whole candidate list crosses the
/// boundary with it, and because a person reads that list with their eyes.
const HOLD_FOUND: Duration = Duration::from_secs(1);

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// The device list, or something shown beside a device.
    Devices,
    /// The transfer list, or something shown beside a transfer.
    Transfers,
    /// The pairing candidate list.
    Found,
}

/// One kind of change, and when it may next be reported.
struct Slot {
    /// True while a change of this kind is waiting.
    waiting: bool,
    /// When this kind was last reported.
    last: Option<Instant>,
    /// How long a change of this kind waits.
    hold: Duration,
}

impl Slot {
    fn new(hold: Duration) -> Self {
        Self {
            waiting: false,
            last: None,
            hold,
        }
    }

    /// How long until this kind may be reported, or `None` when nothing is
    /// waiting.
    fn left(&self, now: Instant) -> Option<Duration> {
        if !self.waiting {
            return None;
        }
        let Some(last) = self.last else {
            return Some(Duration::ZERO);
        };
        Some(
            self.hold
                .saturating_sub(now.saturating_duration_since(last)),
        )
    }

    /// Take this kind's turn when it has come, or when `force` says now.
    fn take(&mut self, now: Instant, force: bool) -> bool {
        let due = match self.left(now) {
            None => return false,
            Some(left) => left.is_zero(),
        };
        if !due && !force {
            return false;
        }
        self.waiting = false;
        self.last = Some(now);
        true
    }
}

/// Everything waiting to be reported.
struct Outbox {
    devices: Slot,
    transfers: Slot,
    found: Slot,
    /// True while a thread is waiting to report what is held back.
    timer: bool,
}

impl Outbox {
    fn slot(&mut self, change: Change) -> &mut Slot {
        match change {
            Change::Devices => &mut self.devices,
            Change::Transfers => &mut self.transfers,
            Change::Found => &mut self.found,
        }
    }
}

/// Which kinds may be reported now.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Due {
    /// The device list changed.
    pub(crate) devices: bool,
    /// The transfer list changed.
    pub(crate) transfers: bool,
    /// The pairing candidate list changed.
    pub(crate) found: bool,
}

impl Due {
    /// True when there is nothing to report.
    pub(crate) fn is_empty(self) -> bool {
        !self.devices && !self.transfers && !self.found
    }
}

/// The listener, and what is waiting to reach it.
pub(crate) struct Notify {
    /// Empty once `stop` has run.
    listener: Mutex<Option<Arc<dyn EngineListener>>>,
    outbox: Mutex<Outbox>,
}

impl Notify {
    /// Hold the listener the app gave the engine.
    pub(crate) fn new(listener: Box<dyn EngineListener>) -> Self {
        Self {
            listener: Mutex::new(Some(Arc::from(listener))),
            outbox: Mutex::new(Outbox {
                devices: Slot::new(HOLD),
                transfers: Slot::new(HOLD),
                found: Slot::new(HOLD_FOUND),
                timer: false,
            }),
        }
    }

    /// The listener, while there is one.
    ///
    /// The guard is dropped before the caller uses what this returns. A
    /// listener that calls back into the engine would otherwise wait on a
    /// lock the engine already holds.
    fn listener(&self) -> Option<Arc<dyn EngineListener>> {
        lock(&self.listener).clone()
    }

    /// Empty the slot. Nothing reaches the app from here on.
    pub(crate) fn close(&self) {
        *lock(&self.listener) = None;
    }

    /// Report a pairing state at once.
    pub(crate) fn pairing(&self, state: &PairingState) {
        if let Some(listener) = self.listener() {
            listener.pairing_changed(state.clone());
        }
    }

    /// Note that something changed. It is reported when its turn comes.
    pub(crate) fn mark(&self, change: Change) {
        lock(&self.outbox).slot(change).waiting = true;
    }

    /// Take every kind whose turn has come. `force` takes them all.
    pub(crate) fn take_due(&self, force: bool) -> Due {
        let now = Instant::now();
        let mut outbox = lock(&self.outbox);
        Due {
            devices: outbox.devices.take(now, force),
            transfers: outbox.transfers.take(now, force),
            found: outbox.found.take(now, force),
        }
    }

    /// Tell the app what `due` names, apart from the candidate list, which
    /// the engine reports itself because it holds the candidates.
    pub(crate) fn send(&self, due: Due) {
        let Some(listener) = self.listener() else {
            return;
        };
        if due.devices {
            listener.devices_changed();
        }
        if due.transfers {
            listener.transfers_changed();
        }
    }

    /// How long until the next kind may be reported.
    ///
    /// Returns `None` when nothing is waiting, and gives the timer back at
    /// the same moment, under the same lock, so the next change knows it has
    /// to start a new one.
    pub(crate) fn next_turn(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut outbox = lock(&self.outbox);
        let soonest = [&outbox.devices, &outbox.transfers, &outbox.found]
            .into_iter()
            .filter_map(|slot| slot.left(now))
            .min();
        if soonest.is_none() {
            outbox.timer = false;
        }
        soonest
    }

    /// True when the caller must start the thread that reports what is held
    /// back. Only one such thread runs at a time.
    pub(crate) fn claim_timer(&self) -> bool {
        let now = Instant::now();
        let mut outbox = lock(&self.outbox);
        let waiting = [&outbox.devices, &outbox.transfers, &outbox.found]
            .into_iter()
            .any(|slot| slot.left(now).is_some());
        if !waiting || outbox.timer {
            return false;
        }
        outbox.timer = true;
        true
    }

    /// Give the timer back, whatever is waiting.
    pub(crate) fn release_timer(&self) {
        lock(&self.outbox).timer = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, Notify};
    use crate::{EngineListener, PairingState};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Counter {
        calls: Arc<AtomicU64>,
    }

    impl EngineListener for Counter {
        fn devices_changed(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }

        fn transfers_changed(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }

        fn pairing_changed(&self, _state: PairingState) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn notify_with_counter() -> (Notify, Arc<AtomicU64>) {
        let calls = Arc::new(AtomicU64::new(0));
        let notify = Notify::new(Box::new(Counter {
            calls: Arc::clone(&calls),
        }));
        (notify, calls)
    }

    #[test]
    fn many_changes_of_one_kind_become_one_report() {
        let (notify, calls) = notify_with_counter();
        for _ in 0..1000 {
            notify.mark(Change::Transfers);
            let due = notify.take_due(false);
            notify.send(due);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a thousand changes in a moment are one report"
        );
    }

    #[test]
    fn nothing_is_reported_once_the_slot_is_empty() {
        let (notify, calls) = notify_with_counter();
        notify.close();
        notify.mark(Change::Devices);
        let due = notify.take_due(true);
        notify.send(due);
        notify.pairing(&PairingState::Idle);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "stop empties the slot, and an empty slot reports nothing"
        );
    }

    #[test]
    fn a_change_that_waits_is_still_reported_in_the_end() {
        let (notify, calls) = notify_with_counter();
        notify.mark(Change::Devices);
        let first = notify.take_due(false);
        notify.send(first);
        notify.mark(Change::Devices);
        // Too soon for a second report.
        let held = notify.take_due(false);
        assert!(held.is_empty(), "the second change waits its turn");
        assert!(notify.next_turn().is_some(), "and something is waiting");
        // What waits is reported in the end, which is what `stop` forces.
        let last = notify.take_due(true);
        notify.send(last);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the last state is told");
    }
}
