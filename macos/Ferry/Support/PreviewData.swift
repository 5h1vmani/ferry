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
    static let reachablePhone = DeviceInfo(
        keyHex: String(repeating: "8c4f2a91", count: 8),
        name: "Pixel 3 XL",
        pairedUnixSecs: 1_757_000_000,
        reachableVia: .usb,
        speedBytesPerSec: 38_000_000,
        lastSeenUnixSecs: nil
    )

    static let idlePhone = DeviceInfo(
        keyHex: String(repeating: "5a90ffc1", count: 8),
        name: "Pixel 7a",
        pairedUnixSecs: 1_756_000_000,
        reachableVia: .wifi,
        speedBytesPerSec: nil,
        lastSeenUnixSecs: nil
    )

    static let unreachablePhone = DeviceInfo(
        keyHex: String(repeating: "3d115be0", count: 8),
        name: "Pixel 6 Pro",
        pairedUnixSecs: 1_750_000_000,
        reachableVia: nil,
        speedBytesPerSec: nil,
        lastSeenUnixSecs: 1_757_100_000
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
            chunksVerified: 54
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
            chunksVerified: 18
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
            chunksVerified: 96
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
            chunksVerified: 10
        ),
    ]

    // MARK: - Snapshots, through the real adapter

    static let deviceSnapshots = EngineAdapter.devices(devices)

    static let transferGroups = EngineAdapter.groups(
        transfers: transfers,
        deviceSpeedBytesPerSec: 38_000_000
    )

    static let advertising = EngineAdapter.presence(
        status: Status(reachable: true, listenPort: 53317, adbPresent: true, mount: nil),
        devices: devices
    )

    static let notAdvertising = EngineAdapter.presence(
        status: Status(reachable: false, listenPort: 53317, adbPresent: true, mount: nil),
        devices: []
    )

    /// A mount the engine cannot report yet (docs/engine-contract.md, item 6),
    /// so the Access section can be previewed before it exists.
    static let mountReady = MountSnapshot(path: "/Volumes/Pixel 3 XL")

    static let roots = [
        SharedRootSnapshot(name: "Desktop", path: "/Users/yantram/Desktop", isWritable: true),
        SharedRootSnapshot(name: "Downloads", path: "/Users/yantram/Downloads", isWritable: true),
    ]

    /// Job 7 turned on and having run. The engine cannot do this yet
    /// (docs/engine-contract.md, item 14), so the preview is the only place
    /// this state can be seen.
    static let autoCopyOn = AutoCopySnapshot(
        isEnabled: true,
        source: "DCIM",
        destination: "/Users/yantram/Downloads/Ferry",
        lastRun: S.automatic.lastRun(files: 43, relative: "2 hours ago"),
        isSupported: true
    )

    /// A placeholder pairing offer. TODO(engine 12): the real payload is
    /// serialised by the engine.
    static let offer = PairingOfferSnapshot(
        payload: Data("ferry:preview".utf8),
        expiresIn: FerryFormat.countdown(seconds: 108),
        isReal: false
    )

    /// Access log entries the engine cannot produce yet
    /// (docs/engine-contract.md, item 13). Both directions, and one rolled-up
    /// folder operation, so the row's four shapes can all be seen.
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
