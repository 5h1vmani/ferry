// The engine's callback into the app.
//
// Every method here is called from an engine thread, never from the thread
// the app called in on. So each one hops to the main actor and then asks
// the model to read the new state. Reading devices() and transfers() is
// local and instant, which is why the engine sends a bare notification and
// lets the app ask again.
//
// This object holds the model, and the model holds the engine, so the
// engine is never dropped. That is the ordinary shape, and it is why the
// app must call stop() on quit. See the crate documentation for
// ferry-runtime.

import Foundation

final class EngineEvents: EngineListener {
    private let model: EngineModel

    init(model: EngineModel) {
        self.model = model
    }

    func devicesChanged() {
        let model = self.model
        Task { @MainActor in
            model.reloadDevices()
        }
    }

    func transfersChanged() {
        let model = self.model
        Task { @MainActor in
            model.reloadTransfers()
        }
    }

    func pairingChanged(state: PairingState) {
        let model = self.model
        Task { @MainActor in
            model.pairingMoved(to: state)
        }
    }

    func accessLogChanged() {
        let model = self.model
        Task { @MainActor in
            model.reloadAccessLog()
        }
    }
}
