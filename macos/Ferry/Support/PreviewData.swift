// Values for the SwiftUI previews only. The whole file is compiled into
// debug builds and nothing else, and no screen of the running app reads it.
// The running app shows what the engine reports and nothing else.
//
// The values use the engine's own types, so a preview shows the same shapes
// a real device produces.

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

    static let unreachablePhone = DeviceInfo(
        keyHex: String(repeating: "3d115be0", count: 8),
        name: "Pixel 6 Pro",
        pairedUnixSecs: 1_750_000_000,
        reachableVia: nil,
        speedBytesPerSec: nil,
        lastSeenUnixSecs: 1_757_100_000
    )

    static let devices = [reachablePhone, unreachablePhone]

    /// One transfer in each state the engine reports.
    static let transfers = [
        TransferInfo(
            id: "1",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "IMG_0512.jpg",
            bytesTotal: 4_800_000_000,
            bytesDone: 2_700_000_000,
            state: .active,
            transport: .usb,
            error: nil
        ),
        TransferInfo(
            id: "2",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "VID_20260910_142233.mp4",
            bytesTotal: 1_900_000_000,
            bytesDone: 900_000_000,
            state: .paused,
            transport: .usb,
            error: .Failed(code: "TransferError::Rpc", detail: nil)
        ),
        TransferInfo(
            id: "3",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "Document.pdf",
            bytesTotal: 4_800_000_000,
            bytesDone: 4_800_000_000,
            state: .done,
            transport: .usb,
            error: nil
        ),
        TransferInfo(
            id: "4",
            deviceKeyHex: reachablePhone.keyHex,
            fileName: "IMG_0410.jpg",
            bytesTotal: 2_200_000_000,
            bytesDone: 500_000_000,
            state: .failed,
            transport: .wifi,
            error: .Failed(code: "TransferError::ChunkFailedVerification", detail: nil)
        ),
    ]
}
#endif
