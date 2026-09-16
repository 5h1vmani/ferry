// Transfers and notifications: the Transfers section's rows, the menu
// bar's running batches, and the system notification for an ending.
//
// Split out of EngineModel.swift. `docs/audits/principles.md`, M6.

import AppKit
import Foundation

extension EngineModel {
    func reloadTransfers() {
        transferInfos = engine?.transfers() ?? []
        // `transfers_changed` covers batches too (docs/engine-contract.md,
        // item 2), so this is where `batches()` is read back as well.
        batchInfos = engine?.batches() ?? []
        // A transfer moving changes a device's speed, which the badge and
        // the menu bar both state.
        refreshPresence()
        updateRevealPaths()
        notifyEndedTransfers()
        updateDockBadge()
        objectWillChange.send()
    }

    /// Caches a finished single-file pull's file on disk, the moment its
    /// transfer reaches Done, so `groups(forDevice:)` can hand the view a
    /// value already known rather than a file system call to make.
    /// `docs/audits/ux-gestures.md`, finding 14.
    private func updateRevealPaths() {
        let candidates = transferInfos.compactMap { transfer -> (id: String, path: String)? in
            guard transfer.batchId == nil, transfer.state == .done, transfer.direction == .pull else {
                return nil
            }
            guard revealPaths[transfer.id] == nil else { return nil }
            return (transfer.id, downloadPath + "/" + transfer.fileName)
        }
        guard !candidates.isEmpty else { return }
        Task.detached { [weak self] in
            let found = candidates.filter { FileManager.default.fileExists(atPath: $0.path) }
            guard !found.isEmpty else { return }
            await self?.storeRevealPaths(found)
        }
    }

    /// Caches each path `updateRevealPaths` found on disk, off the main
    /// actor. `docs/audits/ux-gestures.md`, finding 14.
    private func storeRevealPaths(_ found: [(id: String, path: String)]) {
        for item in found {
            revealPaths[item.id] = item.path
        }
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
    func requestNotificationAuthorizationIfNeeded() {
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

    /// Every transfer for one device, grouped as the Transfers section
    /// shows them.
    func groups(forDevice keyHex: String) -> [TransferGroupSnapshot] {
        EngineAdapter.groups(
            transfers: transferInfos.filter { $0.deviceKeyHex == keyHex },
            batches: batchInfos.filter { $0.deviceKeyHex == keyHex },
            revealPaths: revealPaths
        )
    }
}
