// Pairing: the sheet's state machine, both entry methods, and the
// Offering countdown.
//
// Split out of EngineModel.swift. `docs/audits/principles.md`, M6.

import Foundation

extension EngineModel {
    // MARK: - What the listener calls

    func pairingMoved(to state: PairingState) {
        pairingState = state
        refreshPairing()
    }

    private func refreshPairing() {
        pairing = EngineAdapter.pairing(pairingState, method: pairingMethod)
        updateOfferingTimer()
    }

    /// Ticks the `Offering` countdown once a second.
    ///
    /// `refreshPairing` otherwise only runs when the engine reports a real
    /// state change, so without this the drawn countdown ("1:48") would
    /// freeze between those, rather than counting down on its own.
    private func updateOfferingTimer() {
        guard case .offering = pairing else {
            offeringTimer?.invalidate()
            offeringTimer = nil
            return
        }
        guard offeringTimer == nil else { return }
        offeringTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.refreshPairing()
            }
        }
    }

    // MARK: - Pairing

    /// Enters pairing by one method. Called when the sheet opens and again
    /// if a person switches methods.
    func startPairing(method: PairingEntryMethod) {
        // `docs/engine-contract.md`, item 18: asked here so the system
        // prompt has a reason a person is already acting on. Idempotent,
        // so calling it on every pairing start is safe.
        networkName?.requestAuthorization()
        // startPairingWith refuses a second call while pairing already
        // runs and reports the same state again, which left "Use a
        // pairing code instead" dead once the QR offer was under way.
        // Cancelling first only when there is something to cancel keeps
        // the first, ordinary call unchanged. Matches the phone's fix,
        // d430af2.
        if pairingState != .idle {
            engine?.cancelPairing()
        }
        pairingMethod = method
        engine?.startPairingWith(method: method == .scan ? .qr : .code)
        refreshPairing()
    }

    func pickCandidate(id: String) {
        guard let engine else { return }
        do {
            try engine.pickCandidate(id: id)
        } catch {
            actionError = ThreePartError.from(error, canRetry: false)
        }
    }

    /// Answers the six digits, or the Mac's Pair or Refuse on a scanned
    /// request.
    func confirmPairing(accept: Bool) {
        engine?.confirmPairing(accept: accept)
    }

    func cancelPairing() {
        pairingMethod = nil
        engine?.cancelPairing()
        refreshPairing()
    }
}
