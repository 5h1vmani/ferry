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
// Three exceptions stand: TransportBadge reads `DeviceInfo` in one
// initialiser that builds its own state from it; ThreePartError reads
// `FerryError`, because it is the words for one; and a snapshot may hold an
// engine enum directly, such as `DeviceKind` or `TransferState`, when the
// engine's own cases are already exactly what a view needs, with nothing to
// decide and so nothing to map. DeviceDetail no longer browses `Entry`
// live: its caption names the device only, never the landing folder.
// Naming the folder means calling it, which makes it on the peer's disk,
// so that call happens only inside `send`, on the way to a real push.
// `docs/audits/principles-fixes.md`, finding 2.
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
//
// This file holds the class, its stored state, and the four methods every
// concern needs: init, start, stop, and report. Every other concern is a
// same-named extension in its own file: EngineModel+Devices.swift,
// +Transfers.swift, +Pairing.swift, +Mounting.swift, +Sending.swift, and
// +Settings.swift. `docs/audits/principles.md`, M6.

import AppKit
import Foundation
import SwiftUI

@MainActor
final class EngineModel: ObservableObject {
    // devices, pairing, presence, roots, downloadPath, and trustedNetworks,
    // below, are written only by EngineModel and its own extensions, never
    // by a Screens or Components view. `private(set)` cannot say so: Swift
    // has no way to make a setter private to only the other files of one
    // module, and these six are set from extension files such as
    // EngineModel+Devices.swift, not from this file alone. So the rule is
    // enforced by a grep in `scripts/gate.sh`'s mac mode instead of by the
    // compiler. `docs/audits/principles-fixes.md`, finding 9.
    /// Every paired device, as the screens show it.
    @Published var devices: [DeviceSnapshot] = []
    /// Where the pairing sheet is, for the method a person chose.
    @Published var pairing: PairingScreen = .choosing
    /// Whether this Mac advertises, and what is moving.
    @Published var presence: PresenceSnapshot = .unknown
    /// The folders this Mac serves.
    @Published var roots: [SharedRootSnapshot] = []
    /// How many days an access log entry is kept, from
    /// `access_log_retention_days()`. Nil until the engine has started.
    /// `docs/engine-contract.md`, item 13.
    @Published private(set) var accessLogRetentionDays: UInt32?
    /// Set when the engine could not be built or started. While this is
    /// set, the window shows it instead of the devices.
    @Published private(set) var startError: ThreePartError?
    /// Set when one action failed, such as a pull that could not start.
    @Published var actionError: ThreePartError?
    /// Where pulled files land. Covered by the no-view-writes rule above
    /// `devices`.
    @Published var downloadPath: String
    /// The Wi-Fi networks this Mac trusts. Covered by the no-view-writes
    /// rule above `devices`. `docs/engine-contract.md`, item 18.
    @Published var trustedNetworks: [String] = []
    /// The device selected in the window. Read by `ContentView`, and by
    /// every command and gesture that needs "the selected device":
    /// `targetDevice`, the app menu's Send files and Retry, and pairing's
    /// own confirmation. `docs/ux-fix-plan.md`, item 3, "Device choice".
    @Published var selectedDeviceKeyHex: String?

    /// The engine's own values, kept so a change notification can rebuild
    /// snapshots without asking the engine twice.
    var deviceInfos: [DeviceInfo] = []
    var transferInfos: [TransferInfo] = []
    var batchInfos: [BatchInfo] = []
    var pairingState: PairingState = .idle
    /// Which way in a person chose. A view concern, held here because the
    /// engine is told about it and the sheet may be rebuilt at any moment.
    var pairingMethod: PairingEntryMethod?
    /// Ticks `pairing`'s `Offering` countdown once a second. See
    /// `updateOfferingTimer`.
    var offeringTimer: Timer?
    /// Devices this run has already tried to mount since they last became
    /// reachable. Cleared when a device stops being reachable, so the next
    /// reachable moment gets its own try. `docs/engine-contract.md`, item
    /// 6: "do not retry more than once per reachability change."
    var mountAttempted: Set<String> = []
    /// Each group's state as of the last `reloadTransfers`, so an ending is
    /// announced once, and again if the group ends a second time after a
    /// retry. `docs/ux-fix-plan.md`, item 2.
    var lastGroupStates: [String: TransferState] = [:]
    /// True once `lastGroupStates` has been filled by one `reloadTransfers`.
    /// The first read after `start()` can hold a run's whole stored
    /// history, already Done or Failed; that read seeds the dictionary
    /// without notifying. `docs/audits/ux-gestures.md`, finding 5.
    var hasSeededGroupStates = false
    /// True once `TransferNotifier.requestAuthorization` has been asked
    /// for this run. Asked only from a gesture that starts a transfer —
    /// `pull`, `pullFolder`, `send` — never from `reloadTransfers`, so
    /// stored history from a previous run cannot trigger it at launch.
    /// `docs/ux-fix-plan.md`, item 2; `docs/audits/ux-gestures.md`, finding 5.
    var didRequestNotificationAuthorization = false
    /// A finished single-file pull's file on disk, by transfer id.
    /// Computed once, the moment a group reaches Done, so `DeviceDetail`
    /// never runs a file system call in its body.
    /// `docs/audits/ux-gestures.md`, finding 14.
    var revealPaths: [String: String] = [:]

    var engine: Engine?
    private var events: EngineEvents?
    /// True from the moment `start()` is called until it returns. Closes a
    /// race: `start()` suspends at its first `await`, before `engine` is
    /// set, so a second call could pass `guard engine == nil` while the
    /// first is still reading the data directory and the Keychain. The
    /// second call would then lose the data directory lock, land in
    /// `finishStarting`'s catch, and clear the first call's `engine` and
    /// `events`. `isStarting` makes the guard hold for the whole attempt,
    /// not only its first line. `docs/audits/principles-fixes.md`, finding 1.
    private var isStarting = false
    /// Reads the Wi-Fi network name and hands every change to the engine.
    /// `docs/engine-contract.md`, item 18. Created after `start`, dropped
    /// in `stop`.
    var networkName: NetworkName?

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
    ///
    /// The data directory and the Keychain read both touch disk, so both
    /// run off the main actor in a detached task, the way
    /// `mountReachableDevice` runs its own disk and network work. Building
    /// and starting the engine, and publishing the result, hop back to the
    /// main actor in `finishStarting`.
    func start() async {
        // Two callers can reach here: the window's `.task` in
        // FerryApp.swift, which runs again for a second window on the same
        // shared model, and the Retry control in ContentView.swift. Return
        // at once for either while an attempt is already in flight or the
        // engine is already up.
        guard engine == nil, !isStarting else { return }
        isStarting = true
        defer { isStarting = false }
        startError = nil
        EngineModel.addAdbToPath()
        do {
            let (dataDir, key) = try await Task.detached {
                let dataDir = try EngineModel.makeDataDirectory()
                let key = try KeyStore.loadOrCreate()
                return (dataDir, key)
            }.value
            try finishStarting(dataDir: dataDir, key: key)
        } catch {
            startError = ThreePartError.from(error, canRetry: true)
        }
    }

    /// Builds and starts the engine once the data directory exists and the
    /// key is read, then publishes the first snapshot. Runs on the main
    /// actor: this is where `Engine` and every published property are
    /// touched.
    ///
    /// Held so that an engine which was built but failed to start is
    /// stopped again. It holds the data directory until it is, and the
    /// next try would be refused.
    private func finishStarting(dataDir: String, key: KeyPair) throws {
        var built: Engine?
        do {
            let config = Config(
                dataDir: dataDir,
                sharedRoots: roots.map(EngineModel.engineRoot),
                downloadDir: downloadPath,
                displayName: EngineModel.displayName(),
                listenPort: 0,
                key: key,
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
            accessLogRetentionDays = engine.accessLogRetentionDays()
            reloadDevices()
            reloadTransfers()
            startNetworkReader()
        } catch {
            // Safe to clear `engine` and `events` here only because
            // `isStarting` keeps `start()` from letting a second attempt
            // reach this method while this one is still running. So this
            // catch always belongs to the one attempt that owns these
            // fields, never to a concurrent winner's.
            built?.stop()
            engine = nil
            events = nil
            throw error
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
        accessLogRetentionDays = nil
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
        revealPaths = [:]
        NSApp.dockTile.badgeLabel = nil
    }

    /// Turns any thrown error into the words `actionError` shows. Every
    /// concern's extension calls this from its own catch blocks, so it is
    /// internal, not private.
    func report(_ error: Error) {
        actionError = ThreePartError.from(error, canRetry: false)
    }
}
