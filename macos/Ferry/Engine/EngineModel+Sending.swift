// Moving files to and from a paired device: pulling from it, pushing to
// its landing folder, retrying a failed transfer, and forgetting it.
//
// Split out of EngineModel.swift. `docs/audits/principles.md`, M6.

import Foundation

extension EngineModel {
    // MARK: - Work that talks to the other device

    /// Starts copying one file from a paired device into the shared folder.
    func pull(deviceKeyHex: String, remotePath: String, localName: String) {
        guard let engine else { return }
        requestNotificationAuthorizationIfNeeded()
        Task.detached { [weak self] in
            do {
                _ = try engine.pull(
                    deviceKeyHex: deviceKeyHex,
                    remotePath: remotePath,
                    localName: localName
                )
            } catch {
                await self?.report(error)
            }
        }
    }

    /// Starts copying a whole folder from a paired device into one batch.
    func pullFolder(deviceKeyHex: String, remotePath: String) {
        guard let engine else { return }
        requestNotificationAuthorizationIfNeeded()
        Task.detached { [weak self] in
            do {
                _ = try engine.pullFolder(deviceKeyHex: deviceKeyHex, remotePath: remotePath)
            } catch {
                await self?.report(error)
            }
        }
    }

    // MARK: - Sending files by drop, Send files, or a Finder service

    /// Whether `url` names a folder on disk right now.
    nonisolated private static func isFolder(_ url: URL) -> Bool {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory) else {
            return false
        }
        return isDirectory.boolValue
    }

    /// Starts sending local files to one device's landing folder: a drop
    /// on a device row or the detail pane, "Send files…", or the app
    /// menu's Cmd+O all call this. A folder in `urls` is refused, because
    /// the engine has no way to push one yet. `docs/ux-fix-plan.md`, item
    /// 3.
    func send(urls: [URL], toDevice keyHex: String) {
        // Both drop targets, the Send files panel, and the Finder service
        // all call this, so one guard here covers every entry point.
        // `docs/audits/ux-gestures.md`, finding 4.
        guard urls.allSatisfy(\.isFileURL) else {
            actionError = DropError.nonFileURL.threePart(canRetry: false)
            return
        }
        guard urls.allSatisfy({ !EngineModel.isFolder($0) }) else {
            actionError = DropError.folderNotSupported.threePart(canRetry: false)
            return
        }
        guard let engine else { return }
        requestNotificationAuthorizationIfNeeded()
        let localPaths = urls.map(\.path)
        Task.detached { [weak self] in
            do {
                let folder = try engine.landingFolder(deviceKeyHex: keyHex)
                _ = try engine.pushFiles(deviceKeyHex: keyHex, localPaths: localPaths, remoteFolder: folder)
            } catch {
                await self?.report(error)
            }
        }
    }

    /// Routes URLs the "Send with Ferry" Finder service handed this app.
    /// A URL already inside a device's Finder mount is pulled again,
    /// through the resumable engine rather than the plain Finder copy
    /// that put it there; everything else is pushed to `targetDevice`.
    /// `docs/ux-fix-plan.md`, item 3.
    func sendFromFinder(urls: [URL]) {
        var toPush: [URL] = []
        for url in urls {
            if let (device, remotePath) = mountedLocation(of: url) {
                if EngineModel.isFolder(url) {
                    pullFolder(deviceKeyHex: device.keyHex, remotePath: remotePath)
                } else {
                    pull(deviceKeyHex: device.keyHex, remotePath: remotePath, localName: url.lastPathComponent)
                }
            } else {
                toPush.append(url)
            }
        }
        guard !toPush.isEmpty, let device = targetDevice else { return }
        send(urls: toPush, toDevice: device.keyHex)
    }

    /// The device and remote path for a URL inside that device's Finder
    /// mount, or nil when it is outside every mount. The mount serves each
    /// root at its own top level, so `<mount path>/<root>/<rel>` maps
    /// straight onto the remote path `"<root>/<rel>"`: verified against
    /// `crates/ferry-runtime/src/dav/handlers/browse.rs`, where a listed
    /// child's path is built the same way, and `crates/ferry-core/src/path.rs`,
    /// where a `RemotePath`'s first component is the root name.
    private func mountedLocation(of url: URL) -> (device: DeviceSnapshot, remotePath: String)? {
        let path = url.path
        for device in devices {
            guard let mountPath = mount(forDevice: device.keyHex).path else { continue }
            let prefix = mountPath.hasSuffix("/") ? mountPath : mountPath + "/"
            guard path.hasPrefix(prefix) else { continue }
            return (device, String(path.dropFirst(prefix.count)))
        }
        return nil
    }

    /// Restarts a failed transfer from its resume point.
    func retry(transferId: String) {
        guard let engine else { return }
        Task.detached { [weak self] in
            do {
                try engine.retry(transferId: transferId)
            } catch {
                await self?.report(error)
            }
        }
    }

    /// Restarts every failed transfer in a batch.
    func retryBatch(batchId: String) {
        guard let engine else { return }
        Task.detached { [weak self] in
            do {
                try engine.retryBatch(batchId: batchId)
            } catch {
                await self?.report(error)
            }
        }
    }

    /// Restarts every failed transfer of one device: `retryBatch` for
    /// every failed batch, `retry` for every failed transfer with no
    /// batch. The app menu's Cmd+R. `docs/ux-fix-plan.md`, item 5.
    func retryAllFailed(forDevice keyHex: String) {
        for group in groups(forDevice: keyHex) where group.state == .failed {
            switch group.retryTarget {
            case .transfer(let id): retry(transferId: id)
            case .batch(let id): retryBatch(batchId: id)
            }
        }
    }

    /// Removes a device's key and every transfer record for it.
    func forget(keyHex: String) {
        guard let engine else { return }
        // Unmounted before the key is gone, while `deviceInfos` still holds
        // its mount path. The engine's own `forget` also stops the bridge,
        // but not the OS side of the mount. `docs/engine-contract.md`, item
        // 6.
        unmountAndStopBridge(forDevice: keyHex)
        mountAttempted.remove(keyHex)
        Task.detached { [weak self] in
            do {
                try engine.forget(keyHex: keyHex)
            } catch {
                await self?.report(error)
            }
        }
    }
}
