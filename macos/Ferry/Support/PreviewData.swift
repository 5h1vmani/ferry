// Values for the SwiftUI previews only. The whole file is compiled into
// debug builds and nothing else, and no screen of the running app reads it.
// The running app shows what the engine reports and nothing else.
//
// The engine values use the engine's own types, so a preview shows the same
// shapes a real device produces. The snapshots below are built by the same
// adapter the app uses, not written out by hand, so a preview cannot drift
// from what a screen will really be given.

#if DEBUG
import Foundation

enum PreviewData {
    /// On a cable with Wi-Fi also in reach, so the sidebar's spare transport
    /// line has something to show.
    static let reachablePhone = DeviceInfo(
        keyHex: String(repeating: "8c4f2a91", count: 8),
        name: "Pixel 3 XL",
        pairedUnixSecs: 1_757_000_000,
        reachableVia: .usb,
        speedBytesPerSec: 38_000_000,
        lastSeenUnixSecs: nil,
        availableTransports: [.usb, .wifi],
        kind: .phone,
        mountPath: "/Volumes/Pixel 3 XL"
    )

    static let idlePhone = DeviceInfo(
        keyHex: String(repeating: "5a90ffc1", count: 8),
        name: "Pixel 7a",
        pairedUnixSecs: 1_756_000_000,
        reachableVia: .wifi,
        speedBytesPerSec: nil,
        lastSeenUnixSecs: nil,
        availableTransports: [.wifi],
        kind: .phone,
        mountPath: nil
    )

    static let unreachablePhone = DeviceInfo(
        keyHex: String(repeating: "3d115be0", count: 8),
        name: "Pixel 6 Pro",
        pairedUnixSecs: 1_750_000_000,
        reachableVia: nil,
        speedBytesPerSec: nil,
        lastSeenUnixSecs: 1_757_100_000,
        availableTransports: [],
        kind: .phone,
        mountPath: nil
    )

    static let devices = [reachablePhone, idlePhone, unreachablePhone]

    /// One transfer in each state the engine reports. `chunksTotal` and
    /// `chunksVerified` use a 50,000,000 byte chunk for round numbers; the
    /// real size comes from `ferry-core`, but no preview reads it.
    static let transfers = [
        TransferInfo(
            id: "1",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "IMG_0512.jpg",
            bytesTotal: 4_800_000_000,
            bytesDone: 2_700_000_000,
            state: .active,
            transport: .usb,
            error: nil,
            startedUnixSecs: 1_757_500_000,
            endedUnixSecs: nil,
            direction: .pull,
            speedBytesPerSec: 38_000_000,
            chunksTotal: 96,
            chunksVerified: 54,
            batchId: nil
        ),
        TransferInfo(
            id: "2",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "VID_20260910_142233.mp4",
            bytesTotal: 1_900_000_000,
            bytesDone: 900_000_000,
            state: .paused,
            transport: .usb,
            error: .Failed(code: "TransferError::Rpc", detail: nil),
            startedUnixSecs: 1_757_400_000,
            endedUnixSecs: nil,
            direction: .pull,
            speedBytesPerSec: nil,
            chunksTotal: 38,
            chunksVerified: 18,
            batchId: nil
        ),
        TransferInfo(
            id: "3",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "Document.pdf",
            bytesTotal: 4_800_000_000,
            bytesDone: 4_800_000_000,
            state: .done,
            transport: .usb,
            error: nil,
            startedUnixSecs: 1_757_300_000,
            endedUnixSecs: 1_757_300_180,
            direction: .pull,
            speedBytesPerSec: nil,
            chunksTotal: 96,
            chunksVerified: 96,
            batchId: nil
        ),
        TransferInfo(
            id: "4",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "IMG_0410.jpg",
            bytesTotal: 2_200_000_000,
            bytesDone: 500_000_000,
            state: .failed,
            transport: .wifi,
            error: .Failed(code: "TransferError::ChunkFailedVerification", detail: "14"),
            startedUnixSecs: 1_757_200_000,
            endedUnixSecs: 1_757_200_060,
            direction: .pull,
            speedBytesPerSec: nil,
            chunksTotal: 44,
            chunksVerified: 10,
            batchId: nil
        ),
    ]

    // MARK: - Snapshots, through the real adapter

    static let deviceSnapshots = EngineAdapter.devices(devices)

    static let transferGroups = EngineAdapter.groups(transfers: transfers, batches: [])

    static let advertising = EngineAdapter.presence(
        status: Status(
            reachable: true,
            listenPort: 53317,
            adbPresent: true,
            network: "Home",
            wifiPresence: true
        ),
        devices: devices
    )

    static let notAdvertising = EngineAdapter.presence(
        status: Status(
            reachable: false,
            listenPort: 53317,
            adbPresent: true,
            network: "Home",
            wifiPresence: false
        ),
        devices: []
    )

    /// `reachablePhone` already carries a mount path; this is the same fact,
    /// for a preview that wants the mount alone.
    static let mountReady = MountSnapshot(path: reachablePhone.mountPath)

    static let roots = [
        SharedRootSnapshot(name: "Desktop", path: "/Users/yantram/Desktop", isWritable: true),
        SharedRootSnapshot(name: "Downloads", path: "/Users/yantram/Downloads", isWritable: true),
    ]

    /// Job 7 turned on and having run (docs/engine-contract.md, item 14).
    static let autoCopyOn = AutoCopySnapshot(
        isEnabled: true,
        source: "Internal storage/DCIM",
        destination: "/Users/yantram/Downloads/DCIM",
        lastRun: S.automatic.lastRun(files: 43, relative: "2 hours ago"),
        isSupported: true,
        isRunning: false
    )

    /// Job 7 with a batch still moving, for previewing the Running line.
    static let autoCopyRunning = AutoCopySnapshot(
        isEnabled: true,
        source: "Internal storage/DCIM",
        destination: "/Users/yantram/Downloads/DCIM",
        lastRun: S.automatic.lastRun(files: 43, relative: "2 hours ago"),
        isSupported: true,
        isRunning: true
    )

    /// Sample data for the pairing preview only. A real offer's payload
    /// comes from the engine, in `EngineAdapter.offerSnapshot`.
    static let offer = PairingOfferSnapshot(
        payload: Data("ferry:preview".utf8),
        expiresIn: FerryFormat.countdown(seconds: 108)
    )

    /// Access log entries for previews and screenshots, shaped like what
    /// `engine.accessLog(deviceKeyHex:limit:)` returns (docs/engine-contract.md,
    /// item 13). Both directions, and one rolled-up folder operation, so the
    /// row's four shapes can all be seen.
    static let accessEntries: [AccessEntrySnapshot] = [
        AccessEntrySnapshot(
            id: "a1",
            actor: .peer,
            verb: .read,
            path: "Desktop/Q3 notes.md",
            amount: FerryFormat.bytes(48_000),
            time: "14:31",
            atUnixSecs: 1_757_500_260,
            files: nil
        ),
        AccessEntrySnapshot(
            id: "a2",
            actor: .peer,
            verb: .list,
            path: "Desktop",
            amount: S.accessLog.entries(31),
            time: "14:30",
            atUnixSecs: 1_757_500_200,
            files: nil
        ),
        AccessEntrySnapshot(
            id: "a3",
            actor: .thisDevice,
            verb: .read,
            path: "DCIM/Camera",
            amount: FerryFormat.bytes(4_800_000_000),
            time: "14:12",
            atUnixSecs: 1_757_499_120,
            files: 120
        ),
        AccessEntrySnapshot(
            id: "a4",
            actor: .peer,
            verb: .write,
            path: "Downloads/scan.pdf",
            amount: FerryFormat.bytes(2_000_000),
            time: "09:04",
            atUnixSecs: 1_757_480_640,
            files: nil
        ),
    ]

    /// Grouped by the real adapter, so a preview cannot disagree with a
    /// screen about where midnight falls.
    static let accessDays = EngineAdapter.days(from: accessEntries)
}
#endif
