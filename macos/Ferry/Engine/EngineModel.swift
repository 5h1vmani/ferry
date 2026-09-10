// The one object that owns the engine and publishes what the screens show.
//
// It is created once in FerryApp and reaches every screen through
// environmentObject. Nothing else builds an Engine, because one engine at a
// time may use a data directory: the second is refused. See the crate
// documentation for ferry-runtime.
//
// Which thread runs what:
//
//   - devices() and transfers() are local reads, so they run on the main
//     actor when a callback says something changed.
//   - list, pull, forget, and retry talk to the other device, so each one
//     runs in a detached task and publishes its result back on the main
//     actor.

import Foundation
import SwiftUI

@MainActor
final class EngineModel: ObservableObject {
    /// Every paired device, newest state first read.
    @Published private(set) var devices: [DeviceInfo] = []
    /// Every transfer the engine holds, for every device.
    @Published private(set) var transfers: [TransferInfo] = []
    /// Where pairing is right now.
    @Published private(set) var pairing: PairingState = .idle
    /// Set when the engine could not be built or started. While this is
    /// set, the window shows it instead of the devices.
    @Published private(set) var startError: ThreePartError?
    /// Set when one action failed, such as a pull that could not start.
    @Published var actionError: ThreePartError?
    /// The folder served to paired devices, and where pulled files land.
    @Published private(set) var sharedFolderPath: String

    private var engine: Engine?
    private var events: EngineEvents?

    private static let sharedFolderKey = "sharedFolderPath"
    private static let displayNameLimit = 64

    init() {
        sharedFolderPath = EngineModel.storedSharedFolderPath()
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
            EngineModel.addHomebrewToPath()
            let dataDir = try EngineModel.makeDataDirectory()
            try EngineModel.makeDirectory(at: sharedFolderPath)
            let config = Config(
                dataDir: dataDir,
                sharedRoot: sharedFolderPath,
                displayName: EngineModel.displayName(),
                listenPort: 0,
                key: try KeyStore.loadOrCreate()
            )
            let events = EngineEvents(model: self)
            let engine = try Engine(config: config, listener: events)
            built = engine
            try engine.start()
            self.events = events
            self.engine = engine
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
        devices = []
        transfers = []
        pairing = .idle
    }

    /// Serves a different folder. The engine is stopped first, because one
    /// engine at a time may use the data directory.
    func changeSharedFolder(to path: String) {
        stop()
        sharedFolderPath = path
        UserDefaults.standard.set(path, forKey: EngineModel.sharedFolderKey)
        start()
    }

    // MARK: - What the listener calls

    func reloadDevices() {
        devices = engine?.devices() ?? []
    }

    func reloadTransfers() {
        transfers = engine?.transfers() ?? []
    }

    func pairingMoved(to state: PairingState) {
        pairing = state
    }

    // MARK: - Reading

    /// Every transfer for one device, in the order the engine holds them.
    func transfers(for keyHex: String) -> [TransferInfo] {
        transfers.filter { $0.deviceKeyHex == keyHex }
    }

    /// The speed the engine reports for one device, if bytes are moving.
    func speed(forDevice keyHex: String) -> UInt64? {
        devices.first(where: { $0.keyHex == keyHex })?.speedBytesPerSec
    }

    // MARK: - Pairing

    func startPairing() {
        engine?.startPairing()
    }

    func pickCandidate(id: String) {
        guard let engine else { return }
        do {
            try engine.pickCandidate(id: id)
        } catch {
            actionError = ThreePartError.from(error, canRetry: false)
        }
    }

    func confirmPairing(accept: Bool) {
        engine?.confirmPairing(accept: accept)
    }

    func cancelPairing() {
        engine?.cancelPairing()
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

    private static func storedSharedFolderPath() -> String {
        if let stored = UserDefaults.standard.string(forKey: sharedFolderKey), !stored.isEmpty {
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

    /// A GUI app is launched with a short PATH that has no Homebrew in it.
    /// The engine reads PATH when it is created to find adb, and adb lives
    /// in /opt/homebrew/bin on this Mac. Without this, USB is never
    /// available. See docs/decisions/0010-no-mac-sandbox.md.
    private static func addHomebrewToPath() {
        let homebrew = "/opt/homebrew/bin:/usr/local/bin"
        let current = ProcessInfo.processInfo.environment["PATH"] ?? ""
        if current.contains("/opt/homebrew/bin") {
            return
        }
        let combined = current.isEmpty ? homebrew : homebrew + ":" + current
        setenv("PATH", combined, 1)
    }
}
