// One transfer's progress, in one line (docs/components.md, ProgressLine).
// Never a custom drawn bar: this wraps the platform ProgressView.
//
// The engine moves one file per transfer, so the line states the bytes left
// and the speed. The file count docs/ia.md shows belongs to a folder copy,
// which the engine does not do yet.

import SwiftUI

struct ProgressLine: View {
    let transfer: TransferInfo
    /// The speed the engine reports for this transfer's device, if bytes
    /// are moving. The engine reports speed per device, not per transfer.
    var speedBytesPerSec: UInt64?
    var onRetry: () -> Void = {}

    private var fraction: Double {
        guard transfer.bytesTotal > 0 else { return 0 }
        return Double(transfer.bytesDone) / Double(transfer.bytesTotal)
    }

    private var percent: Int {
        Int((fraction * 100).rounded())
    }

    var body: some View {
        switch transfer.state {
        case .queued:
            Text(S.progressLine.queued)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.textSecondary)

        case .active:
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                ProgressView(value: fraction)
                    .tint(FerryColor.accent)
                Text(activeText)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.text)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(activeText)
            .accessibilityValue(S.progressLine.accessibilityTransferring(percent: percent))

        case .paused:
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                ProgressView(value: fraction)
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
            if let error = transfer.error {
                ErrorBlock(error: ThreePartError(error, canRetry: true), onRetry: onRetry)
            }
        }
    }

    /// "2.1 GB remaining · 38 MB/s". The speed is left out when the engine
    /// reports none, because an unknown part is left out, not guessed.
    private var activeText: String {
        let remaining = transfer.bytesTotal > transfer.bytesDone
            ? transfer.bytesTotal - transfer.bytesDone
            : 0
        var parts = [S.progressLine.bytesRemaining(FerryFormat.bytes(remaining))]
        if let speed = speedBytesPerSec, speed > 0 {
            parts.append(FerryFormat.speed(bytesPerSec: speed))
        }
        return parts.joined(separator: S.common.dotSeparator)
    }

    /// "Paused." and then why and what to do, from the error table. The
    /// engine puts the reason for the pause in the transfer's error.
    private var pausedText: String {
        guard let error = transfer.error else { return S.progressLine.paused }
        let words = ThreePartError(error, canRetry: false)
        return [S.progressLine.paused, words.why, words.whatToDo].joined(separator: " ")
    }

    private var doneText: String {
        FerryFormat.bytes(transfer.bytesTotal)
    }
}

#if DEBUG
#Preview {
    VStack(alignment: .leading, spacing: FerrySpace.s5) {
        ForEach(PreviewData.transfers) { transfer in
            ProgressLine(transfer: transfer, speedBytesPerSec: 38_000_000)
        }
    }
    .padding()
}
#endif
