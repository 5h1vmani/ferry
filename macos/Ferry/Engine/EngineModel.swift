// The one object that owns the engine and publishes what the screens show.
//
// It is created once in FerryApp and reaches every screen through
// environmentObject. Nothing else builds an Engine, because one engine at a
// time may use a data directory: the second is refused. See the crate
// documentation for ferry-runtime.
//
// Views never read an engine type. This model publishes snapshots from
// Model/Snapshot.swift, built by Engine/EngineAdapter.swift. That keeps the
// engine gaps in docs/engine-contract.md inside one file.
//
// Which thread runs what:
//
//   - devices(), transfers(), and access_log() are local reads, so they run
//     on the main actor when a callback says something changed.
//   - list, pull, forget, and retry talk to the other device, so each one
//     runs in a detached task and publishes its result back on the main
//     actor.

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

    /// The engine's own values, kept so a change notification can rebuild
    /// snapshots without asking the engine twice.
    private var deviceInfos: [DeviceInfo] = []
    private var transferInfos: [TransferInfo] = []
    private var batchInfos: [BatchInfo] = []
    private var pairingState: PairingState = .idle
    /// Which way in a person chose. A view concern, held here because the
    /// engine is told about it and the sheet may be rebuilt at any moment.
    private var pairingMethod: PairingMethod?

    private var engine: Engine?
    private var events: EngineEvents?

    /// One stored root: its own keys under `rootsKey`, encoded as JSON so
    /// UserDefaults holds one value rather than three parallel arrays.
    private struct StoredRoot: Codable {
        let name: String
        let path: String
        let writable: Bool
    }

    private static let rootsKey = "sharedRoots"
    private static let downloadPathKey = "downloadPath"
    private static let displayNameLimit = 64

    init() {
        roots = EngineModel.storedRoots()
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
        engine?.stop()
        engine = nil
        events = nil
        deviceInfos = []
        transferInfos = []
        batchInfos = []
        devices = []
        pairingState = .idle
        pairingMethod = nil
        pairing = .choosing
        presence = .unknown
    }

    // MARK: - What the listener calls

    func reloadDevices() {
        deviceInfos = engine?.devices() ?? []
        devices = EngineAdapter.devices(deviceInfos)
        refreshPresence()
    }

    func reloadTransfers() {
        transferInfos = engine?.transfers() ?? []
        // `transfers_changed` covers batches too (docs/engine-contract.md,
        // item 2), so this is where `batches()` is read back as well.
        batchInfos = engine?.batches() ?? []
        // A transfer moving changes a device's speed, which the badge and
        // the menu bar both state.
        refreshPresence()
        objectWillChange.send()
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
    }

    // MARK: - Reading

    /// One device by its key, or nil once it is forgotten.
    func device(keyHex: String) -> DeviceSnapshot? {
        devices.first { $0.keyHex == keyHex }
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
        EngineAdapter.mount(forDevice: keyHex)
    }

    /// Job 7's switch and its lines, for one device.
    func autoCopy(forDevice keyHex: String) -> AutoCopySnapshot {
        EngineAdapter.autoCopy(forDevice: keyHex, downloadDir: downloadPath)
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

    // MARK: - Automatic, job 7

    /// TODO(engine 14): `set_auto_copy` does not exist, so the switch is
    /// disabled in the view and this does nothing. It is here so that the
    /// view's shape does not change when the engine gains it.
    func setAutoCopy(forDevice keyHex: String, enabled: Bool) {
    }

    // MARK: - Pairing

    /// Enters pairing by one method. Called when the sheet opens and again
    /// if a person switches methods.
    func startPairing(method: PairingMethod) {
        pairingMethod = method
        // TODO(engine 12): `start_pairing_with(method)` does not exist, so
        // both methods start the same engine-side flow. The scan method
        // then shows a placeholder code, which is why it is not the only
        // way in.
        engine?.startPairing()
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

    /// Answers the six digits, and — once item 12 lands — the Mac's Pair or
    /// Refuse on a scanned request.
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
        Task.detached { [weak self] in
            do {
                _ = try engine.pullFolder(deviceKeyHex: deviceKeyHex, remotePath: remotePath)
            } catch {
                await self?.report(error)
            }
        }
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

    /// Removes a device's key and every transfer record for it.
    func forget(keyHex: String) {
        guard let engine else { return }
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

    private static func storedRoots() -> [SharedRootSnapshot] {
        guard
            let data = UserDefaults.standard.data(forKey: rootsKey),
            let stored = try? JSONDecoder().decode([StoredRoot].self, from: data),
            !stored.isEmpty
        else {
            return defaultRoots()
        }
        return stored.map {
            SharedRootSnapshot(name: $0.name, path: $0.path, isWritable: $0.writable)
        }
    }

    private static func storeRoots(_ roots: [SharedRootSnapshot]) {
        let stored = roots.map { StoredRoot(name: $0.name, path: $0.path, writable: $0.isWritable) }
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
