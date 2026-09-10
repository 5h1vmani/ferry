// One transfer's progress, in one line (docs/components.md, ProgressLine).
// Never a custom drawn bar: this wraps the platform ProgressView.

import SwiftUI

struct ProgressLine: View {
    let transfer: TransferInfo

    private var fraction: Double {
        guard transfer.bytesTotal > 0 else { return 0 }
        return Double(transfer.bytesDone) / Double(transfer.bytesTotal)
    }

    private var percent: Int {
        Int((fraction * 100).rounded())
    }

    var body: some View {
        switch transfer.state {
        case .queued, .active:
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                ProgressView(value: fraction)
                    .tint(FerryColor.accent)
                Text(lineText)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.text)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(lineText)
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
                ErrorBlock(error: error)
            }
        }
    }

    private var lineText: String {
        let files = S.progressLine.filesProgress(done: transfer.filesDone, total: transfer.filesTotal)
        let remaining = S.progressLine.bytesRemaining(FerryFormat.bytes(transfer.bytesTotal - transfer.bytesDone))
        let speed = transfer.speedBytesPerSec.map { FerryFormat.speed(bytesPerSec: $0) }
        var parts = [files, remaining]
        if let speed {
            parts.append(speed)
        }
        return parts.joined(separator: S.common.dotSeparator)
    }

    private var pausedText: String {
        let reason = transfer.pausedReason ?? S.progressLine.cableDisconnectedReason
        return "\(S.progressLine.paused). \(reason)"
    }

    private var doneText: String {
        S.progressLine.doneSummary(
            files: transfer.filesTotal,
            size: FerryFormat.bytes(transfer.bytesTotal),
            duration: FerryFormat.minutes(transfer.doneDurationSeconds ?? 0)
        )
    }
}

#Preview {
    VStack(alignment: .leading, spacing: FerrySpace.s5) {
        ForEach(SampleState.transfers) { transfer in
            ProgressLine(transfer: transfer)
        }
    }
    .padding()
}
