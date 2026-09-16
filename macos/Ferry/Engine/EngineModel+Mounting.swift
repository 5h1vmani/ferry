// The Finder mount, item 6: mounting a newly reachable device, unmounting
// it, and keeping the stored mount path honest with what Finder shows.
//
// Split out of EngineModel.swift. `docs/audits/principles.md`, M6.

import Foundation

extension EngineModel {
    /// Whether the phone's folders are mounted in Finder, and where.
    func mount(forDevice keyHex: String) -> MountSnapshot {
        EngineAdapter.mount(deviceInfos.first { $0.keyHex == keyHex })
    }

    /// Clears the stored mount path for any device whose path no longer
    /// exists on disk: a person ejecting the volume in Finder, rather than
    /// through this app. `docs/engine-contract.md`, item 6, S9: checked at
    /// every devices change, so a volume the person ejects can be mounted
    /// again on the device's next reachable moment, the same as one this
    /// app unmounted itself.
    func clearEjectedMounts() {
        let mounted = deviceInfos.compactMap { info in
            info.mountPath.map { (keyHex: info.keyHex, path: $0) }
        }
        guard !mounted.isEmpty else { return }
        Task.detached { [weak self] in
            let ejected = mounted.filter { !FileManager.default.fileExists(atPath: $0.path) }
            guard !ejected.isEmpty else { return }
            await self?.clearMountPaths(forDevices: ejected.map(\.keyHex))
        }
    }

    /// Clears the stored mount path for each device in `keyHexes`: a
    /// person ejected the volume in Finder since `clearEjectedMounts` last
    /// checked. Runs on the main actor, since it writes `deviceInfos` and
    /// `devices` and can publish through `report(_:)`.
    /// `docs/engine-contract.md`, item 6, S9.
    private func clearMountPaths(forDevices keyHexes: [String]) {
        let ejected = Set(keyHexes)
        for index in deviceInfos.indices where ejected.contains(deviceInfos[index].keyHex) {
            deviceInfos[index].mountPath = nil
            do {
                try engine?.setMountPath(deviceKeyHex: deviceInfos[index].keyHex, path: nil)
            } catch {
                report(error)
            }
        }
        devices = EngineAdapter.devices(deviceInfos)
    }

    /// Starts the bridge and mounts it for every device that just became
    /// reachable and has no mount yet.
    ///
    /// `docs/engine-contract.md`, item 6: the Mac starts the bridge and
    /// mounts when a phone becomes reachable. `mountAttempted` is how "do
    /// not retry more than once per reachability change" is kept: a device
    /// enters it the moment a try starts, whether or not that try
    /// succeeds, and leaves it only once the device stops being reachable.
    func mountNewlyReachableDevices(from previous: [DeviceInfo], to current: [DeviceInfo]) {
        let previousByKey = Dictionary(uniqueKeysWithValues: previous.map { ($0.keyHex, $0) })
        for device in current {
            guard device.reachableVia != nil else {
                mountAttempted.remove(device.keyHex)
                continue
            }
            let wasReachable = previousByKey[device.keyHex]?.reachableVia != nil
            guard !wasReachable, device.mountPath == nil, !mountAttempted.contains(device.keyHex) else {
                continue
            }
            mountAttempted.insert(device.keyHex)
            mountReachableDevice(device)
        }
    }

    /// Starts one device's bridge, mounts it through NetFS off the main
    /// actor, and reports where the OS put it. On failure this reports the
    /// error through the existing `report(_:)` path and does not retry
    /// itself; `mountNewlyReachableDevices` is what tries again, on the
    /// device's next reachable moment.
    private func mountReachableDevice(_ device: DeviceInfo) {
        guard let engine else { return }
        Task.detached { [weak self] in
            do {
                let endpoint = try engine.mountStart(deviceKeyHex: device.keyHex)
                let path = try FinderMount.mount(endpoint: endpoint, deviceKeyHex: device.keyHex)
                try engine.setMountPath(deviceKeyHex: device.keyHex, path: path)
            } catch {
                await self?.report(error)
            }
        }
    }

    /// Unmounts and stops one device's bridge. Used at `forget` and at
    /// quit; safe to call on a device that was never mounted.
    ///
    /// Clears the stored mount path (S9) once the unmount call is made, so
    /// a device the person ejects, or forgets, is not left reporting a
    /// path Finder no longer shows, and so it can be mounted again on its
    /// next reachable moment.
    func unmountAndStopBridge(forDevice keyHex: String) {
        if let path = deviceInfos.first(where: { $0.keyHex == keyHex })?.mountPath {
            FinderMount.unmount(path: path)
        }
        engine?.mountStop(deviceKeyHex: keyHex)
        do {
            try engine?.setMountPath(deviceKeyHex: keyHex, path: nil)
        } catch {
            report(error)
        }
    }
}
