// One group of transfers as one row of the Transfers section
// (docs/ia.md, Transfers). A single file is a group of one and renders
// the same way, so this view does not care whether the engine has a batch
// object yet.
//
// The row states four things in a fixed order: what is moving, which way,
// how far it has got, and — only when a chunk fact exists — how far down
// the failure goes.

import SwiftUI

struct TransferRow: View {
    let group: TransferGroupSnapshot
    let onRetry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s2) {
            HStack(spacing: FerrySpace.s3) {
                Text(group.label)
                    .font(FerryFont.label)
                    .foregroundStyle(FerryColor.text)
                Spacer()
                Text(originAndDirection)
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
                if group.state == .active, let transport = group.transport {
                    TransportBadge(
                        state: TransportBadgeState(
                            transport: transport,
                            speedBytesPerSec: group.speedBytesPerSec
                        )
                    )
                }
            }

            body(for: group.state)

            if let chunks = group.chunks {
                ChunkDisclosure(chunks: chunks)
            }
        }
        .padding(.vertical, FerrySpace.s1)
    }

    /// The states are the ones in docs/ia.md, Transfers. A paused row
    /// carries no retry control, because the engine resumes it on its own,
    /// and it is never coloured: a paused transfer is not dangerous.
    @ViewBuilder
    private func body(for state: TransferState) -> some View {
        switch state {
        case .queued:
            Text(S.progressLine.queued)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.textSecondary)

        case .active:
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                ProgressView(value: group.fraction)
                    .tint(FerryColor.accent)
                Text(activeText)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.text)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(activeText)
            .accessibilityValue(S.progressLine.accessibilityTransferring(percent: group.percent))

        case .paused:
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                ProgressView(value: group.fraction)
                    .tint(FerryColor.borderStrong)
                Text(pausedText)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(pausedText)

        case .done:
            HStack(spacing: FerrySpace.s1) {
                Image(systemName: FerryIcon.done)
                    .foregroundStyle(FerryColor.text)
                Text(doneText)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.text)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(doneText)

        case .failed:
            if let error = group.error {
                ErrorBlock(error: error, onRetry: onRetry)
            }
        }
    }

    /// "Automatic · Phone to Mac", or just the direction. Origin is a
    /// word in the same type as the direction, because it is the same kind
    /// of fact: how this transfer came to exist (docs/ia.md, Transfers).
    private var originAndDirection: String {
        var parts: [String] = []
        if group.origin == .automatic {
            parts.append(S.transfers.originAutomatic)
        }
        parts.append(directionText)
        return parts.joined(separator: S.common.dotSeparator)
    }

    private var directionText: String {
        switch group.direction {
        case .phoneToMac: return S.transfers.directionPhoneToMac
        case .macToPhone: return S.transfers.directionMacToPhone
        }
    }

    /// "43 of 120 files · 2.1 GB remaining · 38 MB/s". Each part that is
    /// unknown is left out, not guessed: a single-file group states no file
    /// count, and a group with no reported speed states no speed.
    private var activeText: String {
        var parts: [String] = []
        if !group.isSingleFile {
            parts.append(S.transfers.filesProgress(
                done: Int(group.filesDone),
                total: Int(group.filesTotal)
            ))
        }
        let remaining = group.bytesTotal > group.bytesDone
            ? group.bytesTotal - group.bytesDone
            : 0
        parts.append(S.progressLine.bytesRemaining(FerryFormat.bytes(remaining)))
        if let speed = group.speedBytesPerSec, speed > 0 {
            parts.append(FerryFormat.speed(bytesPerSec: speed))
        }
        return parts.joined(separator: S.common.dotSeparator)
    }

    /// "Paused." and then why and what to do, from the error table.
    private var pausedText: String {
        guard let error = group.error else { return S.progressLine.paused }
        return [S.progressLine.paused, error.why, error.whatToDo].joined(separator: " ")
    }

    /// "12 files · 4.8 GB · 3 min". The duration is left out while the
    /// engine carries no timestamps (docs/engine-contract.md, item 9).
    private var doneText: String {
        var parts: [String] = []
        if !group.isSingleFile {
            parts.append(S.transfers.fileCount(Int(group.filesTotal)))
        }
        parts.append(FerryFormat.bytes(group.bytesTotal))
        if let duration = group.duration {
            parts.append(duration)
        }
        return parts.joined(separator: S.common.dotSeparator)
    }
}
