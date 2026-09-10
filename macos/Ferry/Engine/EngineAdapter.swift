// The one place the engine's types are read.
//
// Everything the screens show is a snapshot from Model/Snapshot.swift.
// This file turns the values UniFFI exports into those snapshots, and it is
// the only file in the app that names DeviceInfo, TransferInfo, Entry, or
// PairingState. A view that needed a new engine field would change this
// file and its own body, and nothing in between.
//
// Four items in docs/engine-contract.md have no field in the engine yet: 6,
// 12, 13, and 14. Each one is marked `TODO(engine N)`, where N is its
// item number, and each has a default here that is honest: a missing count
// is absent, not zero, and a missing sentence is left out, not guessed.
// That is the three-part rule from docs/voice.md applied to the boundary
// rather than to prose.
//
//   grep -rn "TODO(engine" macos/
//
// is the list of what is still missing, and it shrinks as items land.

import Foundation

enum EngineAdapter {

    // MARK: - Devices

    /// One device as the sidebar and the detail pane show it.
    static func device(_ info: DeviceInfo) -> DeviceSnapshot {
        DeviceSnapshot(
            keyHex: info.keyHex,
            name: info.name,
            kind: info.kind,
            isReachable: info.reachableVia != nil,
            badge: TransportBadgeState(device: info),
            spareTransport: spareTransport(for: info),
            lastSeen: info.lastSeenUnixSecs.map { S.devices.lastSeen(FerryFormat.relative(unixSecs: $0)) },
            pairedDate: FerryFormat.longDate(unixSecs: info.pairedUnixSecs),
            speedBytesPerSec: info.speedBytesPerSec
        )
    }

    static func devices(_ infos: [DeviceInfo]) -> [DeviceSnapshot] {
        infos.map(device)
    }

    /// A transport that is available but not carrying bytes.
    private static func spareTransport(for info: DeviceInfo) -> Transport? {
        info.availableTransports.first { $0 != info.reachableVia }
    }

    // MARK: - Movement

    /// Every transfer for one device, grouped as the Transfers section
    /// shows them: one row per batch from `batches()`, plus one row per
    /// transfer that names no batch, which is every transfer a single
    /// `pull` made.
    static func groups(transfers: [TransferInfo], batches: [BatchInfo]) -> [TransferGroupSnapshot] {
        let batchGroups = batches.map(group)
        let singleGroups = transfers
            .filter { $0.batchId == nil }
            .map(group)
        return batchGroups + singleGroups
    }

    static func group(_ batch: BatchInfo) -> TransferGroupSnapshot {
        TransferGroupSnapshot(
            id: batch.id,
            label: batch.label,
            direction: direction(batch.direction),
            origin: origin(batch.origin),
            state: batch.state,
            filesDone: batch.filesDone,
            filesTotal: batch.filesTotal,
            bytesDone: batch.bytesDone,
            bytesTotal: batch.bytesTotal,
            speedBytesPerSec: batch.speedBytesPerSec,
            // A batch carries no single transport (docs/engine-contract.md,
            // item 2): its files can move on different ones, so the badge
            // that reads this states none rather than picking one.
            transport: nil,
            // A batch carries no single error either. Its transfers can
            // fail for different reasons, so a failed batch's row states no
            // words until the contract carries one.
            error: nil,
            chunks: nil,
            duration: batch.endedUnixSecs.map {
                FerryFormat.duration(seconds: $0 - batch.startedUnixSecs)
            }
        )
    }

    static func group(_ transfer: TransferInfo) -> TransferGroupSnapshot {
        // A pause is not a failure, so its words carry no retry control:
        // the engine resumes it on its own (docs/ia.md, Transfers).
        let words: ThreePartError? = transfer.error.map {
            ThreePartError($0, canRetry: transfer.state == .failed)
        }

        return TransferGroupSnapshot(
            id: transfer.id,
            label: transfer.fileName,
            direction: direction(transfer.direction),
            // A transfer with no batch is always a manual, single-file
            // pull: the only other origin, automatic under item 14, always
            // makes a batch, so this line can never read `.automatic`, now
            // or once item 14 lands.
            origin: .manual,
            state: transfer.state,
            filesDone: transfer.state == .done ? 1 : 0,
            filesTotal: 1,
            bytesDone: transfer.bytesDone,
            bytesTotal: transfer.bytesTotal,
            speedBytesPerSec: transfer.speedBytesPerSec,
            transport: transfer.transport,
            error: words,
            chunks: chunks(for: transfer),
            duration: transfer.endedUnixSecs.map {
                FerryFormat.duration(seconds: $0 - transfer.startedUnixSecs)
            }
        )
    }

    /// "Manual" or "Automatic", from the engine's own `Origin`.
    private static func origin(_ origin: Origin) -> TransferOrigin {
        switch origin {
        case .manual: return .manual
        case .automatic: return .automatic
        }
    }

    /// "Phone to Mac" for a pull, "Mac to phone" for a push.
    private static func direction(_ direction: Direction) -> TransferDirection {
        switch direction {
        case .pull: return .phoneToMac
        case .push: return .macToPhone
        }
    }

    /// The chunk facts for one transfer, or nil when the size is not known
    /// yet. A verify failure also names its chunk in `error.detail`.
    private static func chunks(for transfer: TransferInfo) -> ChunkFacts? {
        guard transfer.chunksTotal > 0 else { return nil }
        return ChunkFacts(
            verified: transfer.chunksVerified,
            total: transfer.chunksTotal,
            failedIndex: transfer.error.flatMap(failedChunkIndex(in:)),
            isCounted: true
        )
    }

    /// The chunk index a verify failure carries in its detail. The engine
    /// puts a value there, never a sentence, so a detail that is a plain
    /// number is the index.
    private static func failedChunkIndex(in error: FerryError) -> UInt32? {
        switch error {
        case let .Failed(_, detail):
            guard let detail else { return nil }
            return UInt32(detail.trimmingCharacters(in: .whitespaces))
        }
    }

    /// Job 7's switch and its three lines.
    ///
    /// TODO(engine 14): `auto_copy` does not exist. The section renders
    /// with the switch off and disabled, which states the truth: Ferry
    /// cannot do this yet. It does not render as off-but-available, which
    /// would be a small lie.
    static func autoCopy(forDevice keyHex: String, downloadDir: String) -> AutoCopySnapshot {
        AutoCopySnapshot(
            isEnabled: false,
            source: "DCIM",
            destination: downloadDir,
            lastRun: nil,
            isSupported: false
        )
    }

    // MARK: - Presence

    /// What the four presence surfaces show.
    static func presence(
        status: Status,
        devices: [DeviceInfo]
    ) -> PresenceSnapshot {
        // The menu bar states the fastest thing moving, because it has room
        // for one number and that is the one a person is waiting on.
        let moving = devices
            .filter { ($0.speedBytesPerSec ?? 0) > 0 }
            .max { ($0.speedBytesPerSec ?? 0) < ($1.speedBytesPerSec ?? 0) }

        return PresenceSnapshot(
            isAdvertising: status.reachable,
            activeTransport: moving?.reachableVia,
            speedBytesPerSec: moving?.speedBytesPerSec,
            isReportedByEngine: true
        )
    }

    // MARK: - Access, L2

    /// Whether the phone's folders are mounted in Finder, and where.
    ///
    /// TODO(engine 6): nothing reports mount state, and the choice of who
    /// owns it — the engine, or this app through FSKit — is open. Until it
    /// is made there is nothing true to say, so the Access section is
    /// absent rather than showing a negative.
    static func mount(forDevice keyHex: String) -> MountSnapshot {
        .notMounted
    }

    /// The folders this Mac serves, from the engine's own `roots()`.
    static func roots(_ infos: [Root]) -> [SharedRootSnapshot] {
        infos.map { info in
            SharedRootSnapshot(name: info.name, path: info.path, isWritable: info.writable)
        }
    }

    // MARK: - Record, L5

    /// The access log for one device, grouped by day, newest first.
    ///
    /// TODO(engine 13): the engine does not log at the file operations
    /// layer, so there is nothing to read and the screen shows its empty
    /// state. A log derived from transfers is deliberately not built here:
    /// a Finder browse produces no transfer, so it would miss most of what
    /// job 9 exists to record, and a log that is quietly incomplete is
    /// worse than one that is honestly empty.
    static func accessLog(forDevice keyHex: String) -> [AccessDaySnapshot] {
        days(from: [])
    }

    /// Groups entries into days. The one place "Today" is decided, so two
    /// views cannot disagree about where a midnight boundary falls.
    static func days(from entries: [AccessEntrySnapshot], now: Date = Date()) -> [AccessDaySnapshot] {
        guard !entries.isEmpty else { return [] }
        let calendar = Calendar.current
        // Entries arrive newest first and stay in that order inside a day.
        var order: [String] = []
        var byTitle: [String: [AccessEntrySnapshot]] = [:]
        for entry in entries {
            let title = entry.dayTitle(now: now, calendar: calendar)
            if byTitle[title] == nil {
                order.append(title)
                byTitle[title] = []
            }
            byTitle[title]?.append(entry)
        }
        return order.map { AccessDaySnapshot(title: $0, entries: byTitle[$0] ?? []) }
    }

    // MARK: - Pairing, L1

    /// Where the pairing sheet is, from the engine's state and the method a
    /// person chose. One function, so the sheet switches on one value
    /// instead of two.
    ///
    /// TODO(engine 12): two states are missing from `PairingState` and so
    /// cannot be produced here. `Offering { offer }` would replace the
    /// placeholder below, and `Requested { name, transport }` is what the
    /// Mac answers with Pair or Refuse. Until they land, the scan method
    /// shows a code nothing can scan, which is why `isReal` is false and
    /// why the sheet always offers the code method as well.
    static func pairing(_ state: PairingState, method: PairingMethod?) -> PairingScreen {
        switch state {
        case .idle:
            guard let method else { return .choosing }
            return method == .scan ? .offering(offer(expiresUnixSecs: nil)) : .waiting

        case let .waiting(expiresUnixSecs):
            // TODO(engine 12): the engine has no Offering state, so the
            // scan method borrows Waiting and shows a placeholder payload.
            // When `start_pairing_with(Qr)` lands, Offering carries the
            // real bytes and their expiry.
            if method == .scan {
                return .offering(offer(expiresUnixSecs: expiresUnixSecs))
            }
            return .waiting

        case let .found(candidates, expiresUnixSecs):
            // A scan needs nothing to browse: the phone already knows which
            // Mac it scanned. Candidates only reach the code method.
            if method == .scan {
                return .offering(offer(expiresUnixSecs: expiresUnixSecs))
            }
            return .found(candidates.map(candidate))

        case let .code(code, _):
            return .code(digits: FerryFormat.pairingCode(code))

        case let .confirmed(device):
            return .confirmed(keyHex: device.keyHex)

        case let .failed(error):
            return .failed(ThreePartError(error, canRetry: true))
        }
    }

    /// The bytes the Mac renders as a square code.
    ///
    /// TODO(engine 12): a placeholder until the engine serialises the real
    /// payload. `isReal` is false so the view can say so rather than
    /// present an unusable code as usable.
    private static func offer(expiresUnixSecs: Int64?) -> PairingOfferSnapshot {
        PairingOfferSnapshot(
            payload: Data(S.pairing.placeholderPayload.utf8),
            expiresIn: expiresUnixSecs.map {
                FerryFormat.countdown(seconds: $0 - Int64(Date().timeIntervalSince1970))
            },
            isReal: false
        )
    }

    /// "Phone over USB" or "Phone on Wi-Fi · 3F9A" (docs/ia.md,
    /// Pairing, the Mac, by code).
    private static func candidate(_ candidate: PairingCandidate) -> PairingCandidateSnapshot {
        let label: String
        switch candidate.transport {
        case .usb:
            label = S.pairing.candidateUSB
        case .wifi:
            label = S.pairing.candidateWifi(shortCode: candidate.shortCode)
        }
        return PairingCandidateSnapshot(id: candidate.id, label: label)
    }
}

// MARK: - Day titles

extension AccessEntrySnapshot {
    /// "Today", "Yesterday", or "8 September 2026". Decided here rather
    /// than in a view, so that a row renders a title it was given and two
    /// screens cannot disagree about where midnight falls.
    fileprivate func dayTitle(now: Date, calendar: Calendar) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(atUnixSecs))
        if calendar.isDateInToday(date) {
            return S.accessLog.today
        }
        if calendar.isDateInYesterday(date) {
            return S.accessLog.yesterday
        }
        return FerryFormat.longDate(unixSecs: atUnixSecs)
    }
}
