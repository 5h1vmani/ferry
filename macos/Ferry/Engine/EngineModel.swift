// The one object that owns the engine and publishes what the screens show.
//
// It is created once in FerryApp and reaches every screen through
// environmentObject. Nothing else builds an Engine, because one engine at a
// time may use a data directory: the second is refused. See the crate
// documentation for ferry-runtime.
//
// A view mostly does not read an engine type: this model publishes
// snapshots from Model/Snapshot.swift, built by Engine/EngineAdapter.swift,
// which keeps the engine gaps in docs/engine-contract.md inside one file.
// Four exceptions stand: DeviceDetail reads `Entry` for the folder it is
// browsing, live, rather than a cached snapshot of it; TransportBadge reads
// `DeviceInfo` in one initialiser that builds its own state from it;
// ThreePartError reads `FerryError`, because it is the words for one; and a
// snapshot may hold an engine enum directly, such as `DeviceKind` or
// `TransferState`, when the engine's own cases are already exactly what a
// view needs, with nothing to decide and so nothing to map.
//
// Which thread runs what:
//
//   - devices(), transfers(), and access_log() are local reads, so they run
//     on the main actor when a callback says something changed.
//   - list, pull, forget, and retry talk to the other device, so each one
//     runs in a detached task and publishes its result back on the main
//     actor.
//   - Mounting a newly reachable device runs the same way: `mount_start`,
//     the NetFS call in FinderMount.swift, and `set_mount_path` all happen
//     in one detached task, off the main actor.

import AppKit
import Foundation
import SwiftUI

@MainActor
final class EngineModel: ObservableObject {
    /// Every paired device, as the screens show it.
    @Published private(set) var devices: [DeviceSnapshot] = []
    /// Where the pairing sheet is, for the method a person chose.
    @Published private(set) var pairing: PairingScreen = .choosing
    /// Whether this Mac advertises, and what is moving.
    @Published private(set) var presence: PresenceSnapshot = .unknown
    /// The folders this Mac serves.
    @Published private(set) var roots: [SharedRootSnapshot] = []
    /// Set when the engine could not be built or started. While this is
    /// set, the window shows it instead of the devices.
    @Published private(set) var startError: ThreePartError?
    /// Set when one action failed, such as a pull that could not start.
    @Published var actionError: ThreePartError?
    /// Where pulled files land.
    @Published private(set) var downloadPath: String
    /// The Wi-Fi networks this Mac trusts. `docs/engine-contract.md`,
    /// item 18.
    @Published private(set) var trustedNetworks: [String] = []
    /// The device selected in the window. Read by `ContentView`, and by
    /// every command and gesture that needs "the selected device":
    /// `targetDevice`, the app menu's Send files and Retry, and pairing's
    /// own confirmation. `docs/ux-fix-plan.md`, item 3, "Device choice".
    @Published var selectedDeviceKeyHex: String?

    /// The engine's own values, kept so a change notification can rebuild
    /// snapshots without asking the engine twice.
    private var deviceInfos: [DeviceInfo] = []
    private var transferInfos: [TransferInfo] = []
    private var batchInfos: [BatchInfo] = []
    private var pairingState: PairingState = .idle
    /// Which way in a person chose. A view concern, held here because the
    /// engine is told about it and the sheet may be rebuilt at any moment.
    private var pairingMethod: PairingEntryMethod?
    /// Ticks `pairing`'s `Offering` countdown once a second. See
    /// `updateOfferingTimer`.
    private var offeringTimer: Timer?
    /// Devices this run has already tried to mount since they last became
    /// reachable. Cleared when a device stops being reachable, so the next
    /// reachable moment gets its own try. `docs/engine-contract.md`, item
    /// 6: "do not retry more than once per reachability change."
    private var mountAttempted: Set<String> = []
    /// Each group's state as of the last `reloadTransfers`, so an ending is
    /// announced once, and again if the group ends a second time after a
    /// retry. `docs/ux-fix-plan.md`, item 2.
    private var lastGroupStates: [String: TransferState] = [:]
    /// True once `lastGroupStates` has been filled by one `reloadTransfers`.
    /// The first read after `start()` can hold a run's whole stored
    /// history, already Done or Failed; that read seeds the dictionary
    /// without notifying. `docs/audits/ux-gestures.md`, finding 5.
    private var hasSeededGroupStates = false
    /// True once `TransferNotifier.requestAuthorization` has been asked
    /// for this run. Asked only from a gesture that starts a transfer —
    /// `pull`, `pullFolder`, `send` — never from `reloadTransfers`, so
    /// stored history from a previous run cannot trigger it at launch.
    /// `docs/ux-fix-plan.md`, item 2; `docs/audits/ux-gestures.md`, finding 5.
    private var didRequestNotificationAuthorization = false

    private var engine: Engine?
    private var events: EngineEvents?
    /// Reads the Wi-Fi network name and hands every change to the engine.
    /// `docs/engine-contract.md`, item 18. Created after `start`, dropped
    /// in `stop`.
    private var networkName: NetworkName?

    /// One stored root: its own keys under `rootsKey`, encoded as JSON so
    /// UserDefaults holds one value rather than three parallel arrays.
    private struct StoredRoot: Codable {
        let name: String
        let path: String
        let writable: Bool
    }

    /// The versioned shape written under `rootsKey`. `version` lets a
    /// future field change tell a stored value apart from a corrupt one
    /// instead of guessing. `docs/audits/fable-engineering.md`, finding 3.
    private struct StoredRoots: Codable {
        let version: Int
        let roots: [StoredRoot]
    }

    private static let rootsKey = "sharedRoots"
    private static let rootsVersion = 1
    private static let downloadPathKey = "downloadPath"
    private static let displayNameLimit = 64

    init() {
        let stored = EngineModel.storedRoots()
        roots = stored.roots
        if stored.decodeFailed {
            actionError = ThreePartError(
                whatStopped: S.settings.rootsLoadFailedStopped,
                why: S.settings.rootsLoadFailedWhy,
                whatToDo: S.settings.rootsLoadFailedToDo,
                canRetry: false
            )
        }
        downloadPath = EngineModel.storedDownloadPath()
    }

    // MARK: - Starting and stopping

    /// Builds the engine and starts it. Safe to call again after a failure;
    /// the Retry control does exactly that.
    func start() {
        guard engine == nil else { return }
        startError = nil
        // Held so that an engine which was built but failed to start is
        // stopped again. It holds the data directory until it is, and the
        // next try would be refused.
        var built: Engine?
        do {
            EngineModel.addAdbToPath()
            let dataDir = try EngineModel.makeDataDirectory()
            let config = Config(
                dataDir: dataDir,
                sharedRoots: roots.map(EngineModel.engineRoot),
                downloadDir: downloadPath,
                displayName: EngineModel.displayName(),
                listenPort: 0,
                key: try KeyStore.loadOrCreate(),
                kind: .mac
            )
            let events = EngineEvents(model: self)
            let engine = try Engine(config: config, listener: events)
            built = engine
            try engine.start()
            self.events = events
            self.engine = engine
            // `start` is what opens the roots on disk, so this is the
            // engine's own answer, not the one this model guessed at init.
            roots = EngineAdapter.roots(engine.roots())
            reloadDevices()
            reloadTransfers()
            startNetworkReader()
        } catch {
            built?.stop()
            startError = ThreePartError.from(error, canRetry: true)
            engine = nil
            events = nil
        }
    }

    /// Stops every thread the engine started. The app calls this on quit.
    /// Safe to call twice.
    func stop() {
        // Unmount every mounted device before the engine that serves them
        // stops answering. `docs/engine-contract.md`, item 6.
        for device in deviceInfos where device.mountPath != nil {
            unmountAndStopBridge(forDevice: device.keyHex)
        }
        engine?.stop()
        engine = nil
        events = nil
        networkName = nil
        deviceInfos = []
        transferInfos = []
        batchInfos = []
        devices = []
        trustedNetworks = []
        pairingState = .idle
        pairingMethod = nil
        pairing = .choosing
        offeringTimer?.invalidate()
        offeringTimer = nil
        presence = .unknown
        mountAttempted = []
        lastGroupStates = [:]
        hasSeededGroupStates = false
        didRequestNotificationAuthorization = false
        NSApp.dockTile.badgeLabel = nil
    }

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

    /// Clears the stored mount path for any device whose path no longer
    /// exists on disk: a person ejecting the volume in Finder, rather than
    /// through this app. `docs/engine-contract.md`, item 6, S9: checked at
    /// every devices change, so a volume the person ejects can be mounted
    /// again on the device's next reachable moment, the same as one this
    /// app unmounted itself.
    private func clearEjectedMounts() {
        for index in deviceInfos.indices {
            guard let path = deviceInfos[index].mountPath else { continue }
            guard !FileManager.default.fileExists(atPath: path) else { continue }
            deviceInfos[index].mountPath = nil
            try? engine?.setMountPath(deviceKeyHex: deviceInfos[index].keyHex, path: nil)
        }
    }

    func reloadTransfers() {
        transferInfos = engine?.transfers() ?? []
        // `transfers_changed` covers batches too (docs/engine-contract.md,
        // item 2), so this is where `batches()` is read back as well.
        batchInfos = engine?.batches() ?? []
        // A transfer moving changes a device's speed, which the badge and
        // the menu bar both state.
        refreshPresence()
        notifyEndedTransfers()
        updateDockBadge()
        objectWillChange.send()
    }

    /// Every batch moving right now, across every device, for the menu
    /// bar's one line per running batch. `docs/ux-fix-plan.md`, item 2.
    var runningBatches: [TransferGroupSnapshot] {
        EngineAdapter.groups(transfers: transferInfos, batches: batchInfos)
            .filter { $0.state == .active }
    }

    /// Asks for notification authorization the first time a gesture in
    /// this session starts a transfer. Called from `pull`, `pullFolder`,
    /// and `send`, never from `reloadTransfers`: stored history read at
    /// launch is not a gesture. `docs/ux-fix-plan.md`, item 2;
    /// `docs/audits/ux-gestures.md`, finding 5.
    private func requestNotificationAuthorizationIfNeeded() {
        guard !didRequestNotificationAuthorization else { return }
        didRequestNotificationAuthorization = true
        TransferNotifier.requestAuthorization()
    }

    /// Posts one notification for every batch or single transfer that just
    /// moved to Done or Failed since the last read. The first call after
    /// `start()` only seeds `lastGroupStates`: it never notifies, because a
    /// group already Done or Failed there is stored history, not an
    /// ending this run watched happen. `docs/ux-fix-plan.md`, item 2;
    /// `docs/audits/ux-gestures.md`, finding 5.
    private func notifyEndedTransfers() {
        let groups = EngineAdapter.groups(transfers: transferInfos, batches: batchInfos)
        guard hasSeededGroupStates else {
            for group in groups {
                lastGroupStates[group.id] = group.state
            }
            hasSeededGroupStates = true
            return
        }
        for group in groups {
            let previous = lastGroupStates[group.id]
            lastGroupStates[group.id] = group.state
            guard previous != group.state, group.state == .done || group.state == .failed else { continue }
            TransferNotifier.notify(group: group)
        }
    }

    /// Sets the Dock badge to the count of running transfers, and clears
    /// it at zero. `docs/ux-fix-plan.md`, item 2.
    private func updateDockBadge() {
        let running = transferInfos.filter { $0.state == .active }.count
        NSApp.dockTile.badgeLabel = running > 0 ? FerryFormat.badgeCount(running) : nil
    }

    /// `EngineEvents.accessLogChanged` calls this at most once every 250
    /// milliseconds. Nothing is cached here, the way `mount(forDevice:)` and
    /// `autoCopy(forDevice:)` cache nothing: `accessLog(forDevice:)` reads
    /// the engine fresh on every call, so this only has to ask the section
    /// to render again.
    func reloadAccessLog() {
        objectWillChange.send()
    }

    func pairingMoved(to state: PairingState) {
        pairingState = state
        refreshPairing()
    }

    private func refreshPresence() {
        guard let engine else {
            presence = .unknown
            return
        }
        presence = EngineAdapter.presence(status: engine.status(), devices: deviceInfos)
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

    /// Every transfer for one device, grouped as the Transfers section
    /// shows them.
    func groups(forDevice keyHex: String) -> [TransferGroupSnapshot] {
        EngineAdapter.groups(
            transfers: transferInfos.filter { $0.deviceKeyHex == keyHex },
            batches: batchInfos.filter { $0.deviceKeyHex == keyHex }
        )
    }

    /// Whether the phone's folders are mounted in Finder, and where.
    func mount(forDevice keyHex: String) -> MountSnapshot {
        EngineAdapter.mount(deviceInfos.first { $0.keyHex == keyHex })
    }

    /// Job 7's switch and its lines, for one device. Reads the engine
    /// fresh on every call, the same as `accessLog(forDevice:)`: caching
    /// `deviceInfos` would show a run's last count only as stale as the
    /// last `devices_changed` callback happened to be.
    func autoCopy(forDevice keyHex: String) -> AutoCopySnapshot {
        guard let engine else { return .unsupported }
        return EngineAdapter.autoCopy(engine.autoCopy(deviceKeyHex: keyHex))
    }

    /// The access log for one device, grouped by day, newest first. Capped
    /// at the engine's own 1,000 entry limit, which the day grouping never
    /// needs more than for one device's own history.
    func accessLog(forDevice keyHex: String) -> [AccessDaySnapshot] {
        let entries = engine?.accessLog(deviceKeyHex: keyHex, limit: 1000) ?? []
        return EngineAdapter.accessLog(entries)
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
    private func startNetworkReader() {
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

    // MARK: - The Finder mount, item 6

    /// Starts the bridge and mounts it for every device that just became
    /// reachable and has no mount yet.
    ///
    /// `docs/engine-contract.md`, item 6: the Mac starts the bridge and
    /// mounts when a phone becomes reachable. `mountAttempted` is how "do
    /// not retry more than once per reachability change" is kept: a device
    /// enters it the moment a try starts, whether or not that try
    /// succeeds, and leaves it only once the device stops being reachable.
    private func mountNewlyReachableDevices(from previous: [DeviceInfo], to current: [DeviceInfo]) {
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
    private func unmountAndStopBridge(forDevice keyHex: String) {
        if let path = deviceInfos.first(where: { $0.keyHex == keyHex })?.mountPath {
            FinderMount.unmount(path: path)
        }
        engine?.mountStop(deviceKeyHex: keyHex)
        try? engine?.setMountPath(deviceKeyHex: keyHex, path: nil)
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
        // ec25f31.
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

    // MARK: - Shared folders

    /// Serves exactly this set of folders, under the names already known
    /// for a path that was already served, and a name guessed from the
    /// folder itself for a path that is new. No engine restart: `set_roots`
    /// reaches an already-connected peer on its next operation.
    ///
    /// While the engine has not started, such as before storage permission
    /// is granted, this only persists the choice: `start` reads it back.
    func setRoots(_ paths: [String]) {
        guard !paths.isEmpty else { return }
        let known = Dictionary(uniqueKeysWithValues: roots.map { ($0.path, $0) })
        let wanted = paths.map { path in
            known[path] ?? SharedRootSnapshot(
                name: (path as NSString).lastPathComponent,
                path: path,
                isWritable: true
            )
        }
        if let engine {
            do {
                try engine.setRoots(roots: wanted.map(EngineModel.engineRoot))
            } catch {
                report(error)
                return
            }
            roots = EngineAdapter.roots(engine.roots())
        } else {
            roots = wanted
        }
        EngineModel.storeRoots(roots)
    }

    /// Where pulled files land. No engine restart: `set_download_dir` takes
    /// effect for the next pull. While the engine has not started, this
    /// only persists the choice, the same as `setRoots`.
    func setDownloadPath(_ path: String) {
        if let engine {
            do {
                try engine.setDownloadDir(path: path)
            } catch {
                report(error)
                return
            }
        }
        downloadPath = path
        UserDefaults.standard.set(path, forKey: EngineModel.downloadPathKey)
    }

    // MARK: - Work that talks to the other device

    /// Lists one folder on a paired device. This blocks for a round trip,
    /// so it runs off the main thread and the caller awaits it.
    func list(deviceKeyHex: String, remotePath: String) async throws -> [Entry] {
        guard let engine else {
            throw FerryError.Failed(code: "Runtime::NotStarted", detail: nil)
        }
        return try await Task.detached {
            try engine.list(deviceKeyHex: deviceKeyHex, remotePath: remotePath)
        }.value
    }

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

    /// Where a gesture-started push lands on one device: the first root it
    /// lists, in "Download" (docs/engine-contract.md, item 5, "Where a
    /// push lands"). Read only: does not make the folder. Runs the round
    /// trip off the main thread, the same as `list`.
    func landingFolder(forDevice keyHex: String) async throws -> String {
        guard let engine else {
            throw FerryError.Failed(code: "Runtime::NotStarted", detail: nil)
        }
        return try await Task.detached {
            let roots = try engine.list(deviceKeyHex: keyHex, remotePath: "")
            return try EngineModel.landingFolderPath(from: roots)
        }.value
    }

    /// The folder name `landingFolder` and `send` both compute from a
    /// `list("")` call: the first root's name, then "Download".
    nonisolated private static func landingFolderPath(from roots: [Entry]) throws -> String {
        guard let firstRoot = roots.first else {
            throw DropError.noLandingFolder
        }
        return firstRoot.name + "/" + S.drop.downloadFolderName
    }

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
                let roots = try engine.list(deviceKeyHex: keyHex, remotePath: "")
                let folder = try EngineModel.landingFolderPath(from: roots)
                do {
                    try engine.mkdir(deviceKeyHex: keyHex, remotePath: folder)
                } catch let error as FerryError {
                    if case let .Failed(code, _) = error, code == "OpError::AlreadyExists" {
                        // The folder is already there, which is the
                        // outcome this call asked for.
                    } else {
                        throw error
                    }
                }
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

    private func report(_ error: Error) {
        actionError = ThreePartError.from(error, canRetry: false)
    }

    // MARK: - Where things live

    /// Desktop and Downloads, both writable: names a person recognises,
    /// where the first version's single "Ferry" folder was a name Ferry
    /// made up.
    private static func defaultRoots() -> [SharedRootSnapshot] {
        [NSHomeDirectory() + "/Desktop", NSHomeDirectory() + "/Downloads"].map { path in
            SharedRootSnapshot(
                name: (path as NSString).lastPathComponent,
                path: path,
                isWritable: true
            )
        }
    }

    /// No stored value at all is a first run, so it returns the writable
    /// defaults. A stored value that does not decode, whether corrupt or
    /// from a shape this build no longer reads, never falls back to those
    /// writable folders: that would silently share Desktop and Downloads
    /// on a change nobody asked for. It returns an empty list instead and
    /// says so through `decodeFailed`. `docs/audits/fable-engineering.md`,
    /// finding 3.
    private static func storedRoots() -> (roots: [SharedRootSnapshot], decodeFailed: Bool) {
        guard let data = UserDefaults.standard.data(forKey: rootsKey) else {
            return (defaultRoots(), false)
        }
        guard
            let stored = try? JSONDecoder().decode(StoredRoots.self, from: data),
            stored.version == rootsVersion
        else {
            return ([], true)
        }
        guard !stored.roots.isEmpty else {
            return (defaultRoots(), false)
        }
        return (
            stored.roots.map {
                SharedRootSnapshot(name: $0.name, path: $0.path, isWritable: $0.writable)
            },
            false
        )
    }

    private static func storeRoots(_ roots: [SharedRootSnapshot]) {
        let stored = StoredRoots(
            version: rootsVersion,
            roots: roots.map { StoredRoot(name: $0.name, path: $0.path, writable: $0.isWritable) }
        )
        guard let data = try? JSONEncoder().encode(stored) else { return }
        UserDefaults.standard.set(data, forKey: rootsKey)
    }

    /// A `SharedRootSnapshot`, as the engine's `Config` and `set_roots`
    /// take it.
    private static func engineRoot(_ root: SharedRootSnapshot) -> Root {
        Root(name: root.name, path: root.path, writable: root.isWritable)
    }

    private static func storedDownloadPath() -> String {
        if let stored = UserDefaults.standard.string(forKey: downloadPathKey), !stored.isEmpty {
            return stored
        }
        return NSHomeDirectory() + "/Downloads/Ferry"
    }

    /// The engine's own files: paired devices and transfer records.
    private static func makeDataDirectory() throws -> String {
        let path = NSHomeDirectory() + "/Library/Application Support/Ferry"
        try makeDirectory(at: path)
        return path
    }

    private static func makeDirectory(at path: String) throws {
        try FileManager.default.createDirectory(
            atPath: path,
            withIntermediateDirectories: true
        )
    }

    /// The name sent to the other device. At most 64 bytes, which is what
    /// the engine accepts.
    private static func displayName() -> String {
        let name = Host.current().localizedName ?? S.app.defaultDeviceName
        var trimmed = name
        while trimmed.utf8.count > displayNameLimit {
            trimmed.removeLast()
        }
        return trimmed.isEmpty ? S.app.defaultDeviceName : trimmed
    }

    /// A GUI app is launched with a short PATH that has no adb on it. The
    /// engine reads PATH when it is created to find adb. Without this, USB
    /// is never available. See docs/decisions/0010-no-mac-sandbox.md.
    ///
    /// adb is not in Homebrew's bin folder. The Homebrew command line tools
    /// put it under share, and Android Studio puts it under the home
    /// folder. Every known place is added, and the one that exists wins.
    private static func addAdbToPath() {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let known = [
            "/opt/homebrew/share/android-commandlinetools/platform-tools",
            "/usr/local/share/android-commandlinetools/platform-tools",
            home + "/Library/Android/sdk/platform-tools",
            "/opt/homebrew/bin",
            "/usr/local/bin",
        ]
        let current = ProcessInfo.processInfo.environment["PATH"] ?? ""
        let present = Set(current.split(separator: ":").map(String.init))
        let missing = known.filter { !present.contains($0) }
        if missing.isEmpty {
            return
        }
        let prefix = missing.joined(separator: ":")
        let combined = current.isEmpty ? prefix : prefix + ":" + current
        setenv("PATH", combined, 1)
    }
}
