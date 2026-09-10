// Plain structs mirroring what the Rust engine will provide once it is
// linked in. Nothing here talks to the engine yet; SampleState below is the
// only data any screen shows. Names, file names, and other instance values
// are data, not UI copy, so they are written out here rather than routed
// through Strings.swift.

import Foundation

// MARK: - Devices

/// How a device is currently reached, if at all. `nil` on DeviceInfo means
/// not reachable.
enum ReachableVia {
    case usb
    case wifi
}

struct DeviceInfo: Identifiable, Equatable {
    let id: UUID
    let name: String
    let reachableVia: ReachableVia?
    /// Only meaningful while bytes are moving. nil when idle or unreachable.
    let speedBytesPerSec: Int?
    /// When last reachable. nil while currently reachable.
    let lastSeen: Date?
    let pairedDate: Date
    let keyFingerprint: String
}

// MARK: - Transfers

enum TransferState {
    case queued
    case active
    case paused
    case done
    case failed
}

/// The three-part rule from docs/voice.md, as data: what stopped, why, what
/// to do. Any part that is unknown is left out at the call site, never
/// guessed here. Used by both a failed transfer and a failed pairing.
struct ThreePartError: Equatable {
    let whatStopped: String
    let why: String
    let whatToDo: String
    let canRetry: Bool
}

struct TransferInfo: Identifiable {
    let id: UUID
    let fileName: String
    let filesDone: Int
    let filesTotal: Int
    let bytesTotal: Int64
    let bytesDone: Int64
    let speedBytesPerSec: Int?
    let state: TransferState
    let transport: ReachableVia
    /// Set only when state is .paused.
    let pausedReason: String?
    /// Set only when state is .failed.
    let error: ThreePartError?
    /// Set only when state is .done.
    let doneDurationSeconds: Int?
}

// MARK: - Pairing

struct PairingCandidate: Identifiable, Equatable {
    let id = UUID()
    /// "Phone over USB" or "Phone on Wi-Fi · 3F9A" (docs/ia.md, Pairing,
    /// Found). This is per-candidate data, not a fixed piece of UI copy.
    let label: String
}

enum PairingState: Equatable {
    case waiting
    case found([PairingCandidate])
    case code(String)
    case confirmed
    case failed(ThreePartError)
}

// MARK: - Sample state

/// The only data any screen shows. Two shapes: `populated`, with the
/// devices and transfers docs/ia.md describes, and `empty`, the first-run
/// state. ContentView's debug switcher picks between them.
enum SampleState {
    static let reachablePhone = DeviceInfo(
        id: UUID(),
        name: "Pixel 3 XL",
        reachableVia: .usb,
        speedBytesPerSec: 38_000_000,
        lastSeen: nil,
        pairedDate: Calendar.current.date(byAdding: .day, value: -42, to: Date())!,
        keyFingerprint: "8C:4F:2A:91:D6:03:B7:5E:1F:9A:6C:22:E4:B8:70:11"
    )

    static let unreachablePhone = DeviceInfo(
        id: UUID(),
        name: "Pixel 6 Pro",
        reachableVia: nil,
        speedBytesPerSec: nil,
        lastSeen: Calendar.current.date(byAdding: .hour, value: -2, to: Date())!,
        pairedDate: Calendar.current.date(byAdding: .day, value: -120, to: Date())!,
        keyFingerprint: "3D:11:5B:E0:47:9C:2A:F6:88:0D:C1:34:AE:59:7B:02"
    )

    static let devices: [DeviceInfo] = [reachablePhone, unreachablePhone]

    /// Every transfer belongs to `reachablePhone`, which is the device with
    /// an active USB link. One of each state docs/ia.md lists.
    static let transfers: [TransferInfo] = [
        TransferInfo(
            id: UUID(),
            fileName: "IMG_0512.jpg",
            filesDone: 3,
            filesTotal: 120,
            bytesTotal: 4_800_000_000,
            bytesDone: 2_700_000_000,
            speedBytesPerSec: 38_000_000,
            state: .active,
            transport: .usb,
            pausedReason: nil,
            error: nil,
            doneDurationSeconds: nil
        ),
        TransferInfo(
            id: UUID(),
            fileName: "VID_20260910_142233.mp4",
            filesDone: 12,
            filesTotal: 40,
            bytesTotal: 1_900_000_000,
            bytesDone: 900_000_000,
            speedBytesPerSec: nil,
            state: .paused,
            transport: .usb,
            pausedReason: S.progressLine.cableDisconnectedReason,
            error: nil,
            doneDurationSeconds: nil
        ),
        TransferInfo(
            id: UUID(),
            fileName: "Document.pdf",
            filesDone: 120,
            filesTotal: 120,
            bytesTotal: 4_800_000_000,
            bytesDone: 4_800_000_000,
            speedBytesPerSec: nil,
            state: .done,
            transport: .usb,
            pausedReason: nil,
            error: nil,
            doneDurationSeconds: 180
        ),
        TransferInfo(
            id: UUID(),
            fileName: "IMG_0410.jpg",
            filesDone: 14,
            filesTotal: 60,
            bytesTotal: 2_200_000_000,
            bytesDone: 500_000_000,
            speedBytesPerSec: nil,
            state: .failed,
            transport: .wifi,
            pausedReason: nil,
            error: ThreePartError(
                whatStopped: "Chunk 14 of IMG_0410.jpg failed to verify.",
                why: "The file changed on the phone during transfer.",
                whatToDo: "Send it again.",
                canRetry: true
            ),
            doneDurationSeconds: nil
        ),
    ]

    /// One representative value per PairingState case (docs/ia.md,
    /// Pairing), in the order a real pairing moves through them. The
    /// debug picker in PairingSheet cycles through these since no engine
    /// drives real transitions yet.
    static let pairingWaiting = PairingState.waiting

    static let pairingFound = PairingState.found([
        PairingCandidate(label: "Phone over USB"),
        PairingCandidate(label: "Phone on Wi-Fi · 3F9A"),
    ])

    static let pairingCode = PairingState.code("481 920")

    static let pairingConfirmed = PairingState.confirmed

    static let pairingFailed = PairingState.failed(
        ThreePartError(
            whatStopped: "Pairing stopped.",
            why: "The codes did not match.",
            whatToDo: "Try again.",
            canRetry: true
        )
    )

    static let allPairingStates: [(label: String, state: PairingState)] = [
        (S.debug.pairingStateWaiting, pairingWaiting),
        (S.debug.pairingStateFound, pairingFound),
        (S.debug.pairingStateCode, pairingCode),
        (S.debug.pairingStateConfirmed, pairingConfirmed),
        (S.debug.pairingStateFailed, pairingFailed),
    ]
}
