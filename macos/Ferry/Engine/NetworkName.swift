// Reads the Wi-Fi network name and reports every change to it.
//
// docs/engine-contract.md, item 18: only the app can read this, so the
// engine asks the app to report it. This file is the one place that
// reads CoreWLAN and CoreLocation; EngineModel only sees a name and a
// closure.
//
// On macOS 14 and later, CWInterface.ssid() answers nil until this app
// is authorised for location, so this object also owns the
// CLLocationManager that asks for it. A refusal is not an error: the
// name stays nil, the same as Wi-Fi being off or unreadable for any
// other reason.
//
// Never polls. The name is read once at authorisation time and again
// only when CoreWLAN's own ssidDidChange event fires, or when the
// location authorisation itself changes, which can turn a nil into a
// real name with the network never having changed at all.

import CoreLocation
import CoreWLAN
import Foundation

final class NetworkName: NSObject {
    /// Called with the current name on every change. Unknown, empty, or
    /// unreadable is nil, never guessed.
    private let onChange: (String?) -> Void

    private let client: CWWiFiClient
    private let locationManager: CLLocationManager

    /// False when `startMonitoringEvent` failed to start: a later network
    /// change would never reach `onChange`, so `currentName` answers nil
    /// from then on rather than a name that could go stale forever.
    /// `docs/audits/principles.md`, M18.
    private var isMonitoring = true

    /// The name right now, read fresh rather than cached: `EngineModel`
    /// calls this once, right after the engine starts.
    var currentName: String? {
        guard isMonitoring else { return nil }
        guard let ssid = client.interface()?.ssid(), !ssid.isEmpty else { return nil }
        return ssid
    }

    init(client: CWWiFiClient = .shared(), onChange: @escaping (String?) -> Void) {
        self.client = client
        self.locationManager = CLLocationManager()
        self.onChange = onChange
        super.init()
        locationManager.delegate = self
        client.delegate = self
        do {
            try client.startMonitoringEvent(with: .ssidDidChange)
        } catch {
            // Reported the same way Settings already states a refused
            // location permission: the network name cannot be read.
            isMonitoring = false
        }
    }

    deinit {
        // Errors are swallowed here the same way FinderMount.unmount
        // swallows them: this runs as EngineModel.stop() tears the model
        // down, presence is reset right after, and nothing is left to
        // report a failure to.
        try? client.stopMonitoringEvent(with: .ssidDidChange)
    }

    /// Asks the system for location authorisation, so `CWInterface.ssid()`
    /// starts answering. `EngineModel` calls this the first time pairing
    /// starts. Idempotent: once a decision exists, this asks nothing
    /// again, so it is safe to call on every pairing start rather than
    /// tracking "first time" a second place.
    func requestAuthorization() {
        guard locationManager.authorizationStatus == .notDetermined else { return }
        locationManager.requestAlwaysAuthorization()
    }
}

extension NetworkName: CWEventDelegate {
    func ssidDidChangeForWiFiInterface(withName interfaceName: String) {
        onChange(currentName)
    }
}

extension NetworkName: CLLocationManagerDelegate {
    /// Authorisation changing can change whether `ssid()` answers even
    /// though the network itself did not change, so this counts as a
    /// change too.
    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        onChange(currentName)
    }
}
