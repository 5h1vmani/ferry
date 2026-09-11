// The shape the screens bind to.
//
// Every view in Ferry reads one of these values and nothing else. None of
// them is a type the engine exports: the engine's own types cross the
// UniFFI boundary and are mapped once, in Engine/EngineAdapter.swift.
//
// Two reasons this layer exists rather than views reading DeviceInfo and
// TransferInfo directly.
//
// The first is that three items in docs/engine-contract.md have no field in
// the engine yet: 6, 12, and 14. Each one has a field here and a marked
// default in the adapter, so the app builds and renders today, and closing a
// gap is a change in one file with no view touched.
//
// The second is that a snapshot holds what a view needs in the form the
// view needs it. A device's badge state, a log's day grouping, and a
// transfer's origin line are decided once, by the adapter, and not three
// times by three views.

import Foundation

// MARK: - Presence

/// Whether this device advertises and accepts connections, and what that
/// costs when it does not. Job 5's only control. L0 in docs/ia.md.
struct PresenceSnapshot: Equatable {
    let isAdvertising: Bool
    /// The transport carrying bytes right now, if any, and how fast.
    let activeTransport: Transport?
    let speedBytesPerSec: UInt64?

    /// True once `EngineAdapter.presence` has built this snapshot from a
    /// real `status()`. False only in `.unknown`, before `start()` has
    /// produced a first one. A view does not read this to change what it
    /// shows: the words are the same either way.
    let isReportedByEngine: Bool

    static let unknown = PresenceSnapshot(
        isAdvertising: false,
        activeTransport: nil,
        speedBytesPerSec: nil,
        isReportedByEngine: false
    )
}

// MARK: - Device

/// One paired device, as the Devices list and the detail pane show it.
struct DeviceSnapshot: Equatable, Identifiable {
    let keyHex: String
    let name: String
    /// What it said in `hello` at pairing time. The engine's own
    /// `DeviceKind`, used directly rather than copied into an app type: it
    /// already has exactly the two cases a peer can be.
    let kind: DeviceKind
    let isReachable: Bool
    let badge: TransportBadgeState
    /// A transport that is available but not carrying bytes. Stated beside
    /// the badge so a pulled cable is not a surprise.
    let spareTransport: Transport?
    /// "Last seen 2 hours ago", already formatted. Nil while reachable.
    let lastSeen: String?
    let pairedDate: String
    let speedBytesPerSec: UInt64?

    var id: String { keyHex }
}

// MARK: - Access, L2

/// Whether the phone's folders are mounted in Finder, and where. Nothing
/// reports it yet (docs/engine-contract.md, item 6), so this is empty and the
/// section is absent.
struct MountSnapshot: Equatable {
    /// "/Volumes/Pixel 3 XL" while mounted.
    let path: String?

    var isReady: Bool { path != nil }

    static let notMounted = MountSnapshot(path: nil)
}

/// One folder this Mac serves, under the name a peer sees. Desktop and
/// Downloads by default (docs/engine-contract.md, item 15).
struct SharedRootSnapshot: Equatable, Identifiable {
    /// "Desktop". The first segment of every path the peer asks for.
    let name: String
    let path: String
    let isWritable: Bool

    var id: String { path }
}

// MARK: - Movement, L3

/// Which way bytes are moving, from the engine's own `Direction`
/// (docs/engine-contract.md, item 4). Always `.phoneToMac` until push, item
/// 5, lands: the core can only pull.
enum TransferDirection: Equatable {
    case phoneToMac
    case macToPhone
}

/// Why a batch exists: a person asked, or Ferry decided. Job 7's fact, and
/// the reason a row can say "Automatic" instead of leaving a person to
/// wonder who asked for it (docs/engine-contract.md, item 14).
enum TransferOrigin: Equatable {
    case manual
    case automatic
}

/// How a failed group's row is retried: the whole batch, or the one
/// transfer it stands for. Decided once, by the adapter that already knows
/// which `BatchInfo` or `TransferInfo` a group came from, so a view calls
/// the right engine method without inspecting either engine type itself
/// (docs/engine-contract.md, item 2).
enum TransferRetryTarget: Equatable {
    case transfer(id: String)
    case batch(id: String)
}

/// Several transfers started by one action, shown as one row: "DCIM/Camera,
/// 120 files". A single file is a group of one and renders the same way.
///
/// Built from a `BatchInfo` for a folder copy, and from one `TransferInfo`
/// for a transfer no batch names (docs/engine-contract.md, item 2). Either
/// way this type is the same, so nothing above the adapter has to know
/// which one it is looking at.
struct TransferGroupSnapshot: Equatable, Identifiable {
    let id: String
    /// "DCIM/Camera, 120 files", or one file's name.
    let label: String
    let direction: TransferDirection
    let origin: TransferOrigin
    let state: TransferState
    let filesDone: UInt32
    let filesTotal: UInt32
    let bytesDone: UInt64
    let bytesTotal: UInt64
    let speedBytesPerSec: UInt64?
    let transport: Transport?
    /// The words for a failure or a pause, already looked up.
    let error: ThreePartError?
    /// Present only where the engine holds a chunk-level fact.
    let chunks: ChunkFacts?
    /// How long a finished group took, already formatted from the engine's
    /// own timestamps (docs/engine-contract.md, item 9). Nil until the
    /// group ends.
    let duration: String?
    /// Which engine call a retry on this row makes.
    let retryTarget: TransferRetryTarget

    var fraction: Double {
        guard bytesTotal > 0 else { return 0 }
        return Double(bytesDone) / Double(bytesTotal)
    }

    var percent: Int {
        Int((fraction * 100).rounded())
    }

    /// True when the group stands for exactly one file: a transfer with no
    /// batch, or a batch of one.
    var isSingleFile: Bool {
        filesTotal <= 1
    }
}

/// The bottom of the depth axis in docs/ia.md. Shown by the chunk
/// disclosure, from the engine's own chunk counts (docs/engine-contract.md,
/// item 7). Present once the transfer's size is known, which is every
/// transfer past its first `stat`.
struct ChunkFacts: Equatable {
    let verified: UInt32
    let total: UInt32
    /// The chunk named in the error, when one was.
    let failedIndex: UInt32?
}

/// Job 7: whether Ferry copies new photos from one device on its own, and
/// what it last did (docs/engine-contract.md, item 14).
struct AutoCopySnapshot: Equatable {
    let isEnabled: Bool
    /// The peer folder watched. "DCIM" in phase 2.
    let source: String
    /// Where copies land, as a path a person recognises.
    let destination: String
    /// "Last copied 43 files, 2 hours ago." Nil before the first run.
    let lastRun: String?
    /// False while the engine has no auto copy at all, so the switch is
    /// shown disabled rather than pretending to work.
    let isSupported: Bool

    static let unsupported = AutoCopySnapshot(
        isEnabled: false,
        source: "DCIM",
        destination: "",
        lastRun: nil,
        isSupported: false
    )
}

// MARK: - Record, L5

/// Who performed the operation.
enum AccessActor: Equatable {
    /// The peer, on this device's files.
    case peer
    /// This device, on the peer's files.
    case thisDevice
}

/// One file operation, as one row of the access log. Job 9.
///
/// `verb` is the engine's own `AccessVerb`, used directly rather than
/// mirrored here: its eight cases are already the file operations layer's
/// own words, so a second, identically shaped type would only be a second
/// name for the same fact.
struct AccessEntrySnapshot: Equatable, Identifiable {
    let id: String
    let actor: AccessActor
    let verb: AccessVerb
    /// Root-relative, beginning with the root name: "Desktop/Q3 notes.md".
    let path: String
    /// "48 KB", "31 entries", or nil when neither applies. Already
    /// formatted, because two views must not round differently.
    let amount: String?
    /// "14:31".
    let time: String
    /// When it happened. Kept as the raw value, not only as `time`,
    /// because the day grouping needs a date and a formatted clock time
    /// cannot be grouped.
    let atUnixSecs: Int64
    /// How many files a rolled-up folder operation covered.
    let files: UInt32?
}

/// One day of the access log, newest first. The adapter decides the title,
/// so "Today" is not computed in two places.
struct AccessDaySnapshot: Equatable, Identifiable {
    /// "Today", "Yesterday", or "8 September 2026".
    let title: String
    let entries: [AccessEntrySnapshot]

    var id: String { title }
}

// MARK: - Pairing, L1

/// Which way in a person chose. Pairing is a once-ever job, so two ways is
/// one screen more, not two things to maintain.
///
/// Named apart from the engine's own `PairingMethod` (`Code`/`Qr`, in
/// `Generated/ferry_runtime.swift`), which this maps to only at the one
/// call into the engine, `EngineModel.startPairing(method:)`. The two do
/// not merge: this one is UI state that exists before the sheet has called
/// the engine at all, such as the moment `.choosing` shows both buttons.
enum PairingEntryMethod: Equatable {
    case scan
    case code
}

/// What the Mac renders as a square code. The payload is opaque: the app
/// draws the bytes and does not parse them.
struct PairingOfferSnapshot: Equatable {
    let payload: Data
    /// "1:48", counted down from the engine's own deadline
    /// (docs/engine-contract.md, item 10). Nil only for the moment before
    /// any pairing state has arrived.
    let expiresIn: String?
}

/// One device the Mac could pair with, in the code method's Found state.
struct PairingCandidateSnapshot: Equatable, Identifiable {
    let id: String
    /// "Phone over USB" or "Phone on Wi-Fi · 3F9A".
    let label: String
}

/// Where the pairing sheet is. One enum for both methods, because both end
/// in the same place and every screen after pairing is untouched.
enum PairingScreen: Equatable {
    /// Neither method chosen yet.
    case choosing
    /// Scan method: the Mac shows a code and waits.
    case offering(PairingOfferSnapshot)
    /// Scan method: a phone scanned it and is asking. One named question,
    /// two answers, no digits.
    case requested(name: String, transport: Transport)
    /// Code method: looking for a phone.
    case waiting
    /// Code method: candidates to pick from.
    case found([PairingCandidateSnapshot])
    /// Code method: the six digits, grouped three and three. PairingCode
    /// spells them out for VoiceOver itself, so they are not spelled here.
    case code(digits: String)
    /// Shared by both methods.
    case confirmed(keyHex: String)
    case failed(ThreePartError)
}
