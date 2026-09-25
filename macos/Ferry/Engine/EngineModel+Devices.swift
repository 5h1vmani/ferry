// Devices and presence: the paired-devices list, per-device reads, Wi-Fi
// presence, trusted networks, and job 7's automatic copy switch.
//
// Split out of EngineModel.swift. `docs/audits/principles.md`, M6.

import Foundation

extension EngineModel {
    // MARK: - What the listener calls

    func reloadDevices() {
        let previous = deviceInfos
        deviceInfos = engine?.devices() ?? []
        clearEjectedMounts()
        devices = EngineAdapter.devices(deviceInfos)
        // The trusted list changes fire the same callback as a device
        // change (docs/engine-contract.md, item 18), so this is read back
        // here rather than on a callback of its own.
        trustedNetworks = engine?.trustedNetworks() ?? []
        refreshPresence()
        mountNewlyReachableDevices(from: previous, to: deviceInfos)
    }

    /// `EngineEvents.accessLogChanged` calls this at most once every 250
    /// milliseconds, when the log can really have changed. The engine's
    /// callback carries no device key, so every device a screen has already
    /// read this run is refreshed; a device nothing has shown yet is left
    /// alone; its first read will fetch fresh.
    func reloadAccessLog() {
        for keyHex in accessLogsByDevice.keys {
            fetchAccessLog(forDevice: keyHex)
        }
        objectWillChange.send()
    }

    func refreshPresence() {
        guard let engine else {
            presence = .unknown
            return
        }
        presence = EngineAdapter.presence(status: engine.status(), devices: deviceInfos)
    }

    // MARK: - Reading

    /// One device by its key, or nil once it is forgotten.
    func device(keyHex: String) -> DeviceSnapshot? {
        devices.first { $0.keyHex == keyHex }
    }

    /// The device a gesture or a command with no device of its own acts
    /// on: the only paired device; when more than one is paired, the one
    /// selected in the window, else the first reachable one.
    /// `docs/ux-fix-plan.md`, item 3, "Device choice".
    var targetDevice: DeviceSnapshot? {
        if devices.count == 1 {
            return devices.first
        }
        if let keyHex = selectedDeviceKeyHex, let device = device(keyHex: keyHex) {
            return device
        }
        return devices.first { $0.isReachable }
    }

    /// Job 7's switch and its lines, for one device. Reads the engine
    /// fresh on every call, the same as `accessLog(forDevice:)`: caching
    /// `deviceInfos` would show a run's last count only as stale as the
    /// last `devices_changed` callback happened to be.
    func autoCopy(forDevice keyHex: String) -> AutoCopySnapshot {
        guard let engine else { return .unsupported }
        return EngineAdapter.autoCopy(engine.autoCopy(deviceKeyHex: keyHex))
    }

    /// The access log for one device, grouped by day, newest first. Read
    /// from the cache: cheap enough to call from a view body on every
    /// redraw, because a redraw is not when the log can have changed.
    /// `reloadAccessLog()` is. `docs/audits/oss-looks.md`, its ALERT on
    /// this file.
    func accessLog(forDevice keyHex: String) -> [AccessDaySnapshot] {
        if let cached = accessLogsByDevice[keyHex] {
            return cached
        }
        return fetchAccessLog(forDevice: keyHex)
    }

    /// Reads the engine, capped at its own 1,000 entry limit, which the day
    /// grouping never needs more than for one device's own history. Caches
    /// the result and returns it.
    @discardableResult
    private func fetchAccessLog(forDevice keyHex: String) -> [AccessDaySnapshot] {
        let entries = engine?.accessLog(deviceKeyHex: keyHex, limit: 1000) ?? []
        let days = EngineAdapter.accessLog(entries)
        accessLogsByDevice[keyHex] = days
        return days
    }

    // MARK: - Presence

    /// Turns advertising on or off. Job 5's only control.
    func setAdvertising(_ on: Bool) {
        engine?.setReachable(on: on)
        refreshPresence()
    }

    // MARK: - Networks, item 18

    /// Creates the Wi-Fi network name reader and hands the engine its
    /// first reading. Called once, right after `start`; every later
    /// change the reader reports is handed to `setNetwork` the same way,
    /// off the main actor, hopped back onto it.
    func startNetworkReader() {
        let reader = NetworkName { [weak self] name in
            Task { @MainActor in
                self?.engine?.setNetwork(name: name)
            }
        }
        networkName = reader
        engine?.setNetwork(name: reader.currentName)
    }

    /// Trusts a Wi-Fi network name: Wi-Fi presence stays on there without
    /// a pairing in progress. `docs/engine-contract.md`, item 18.
    func trustNetwork(_ name: String) {
        guard let engine else { return }
        do {
            try engine.trustNetwork(name: name)
            trustedNetworks = engine.trustedNetworks()
            refreshPresence()
        } catch {
            report(error)
        }
    }

    /// Removes a name from the trusted list.
    func forgetNetwork(_ name: String) {
        guard let engine else { return }
        do {
            try engine.forgetNetwork(name: name)
            trustedNetworks = engine.trustedNetworks()
            refreshPresence()
        } catch {
            report(error)
        }
    }

    // MARK: - Automatic, job 7

    /// Turns job 7's switch on or off for one device.
    ///
    /// No engine restart, and no `Task.detached`: `set_auto_copy` only
    /// persists the choice and, when it turns the switch on for a reachable
    /// device, starts a run on a thread of the engine's own. Neither blocks
    /// this call.
    func setAutoCopy(forDevice keyHex: String, enabled: Bool) {
        guard let engine else { return }
        do {
            try engine.setAutoCopy(deviceKeyHex: keyHex, enabled: enabled)
        } catch {
            report(error)
        }
    }
}
