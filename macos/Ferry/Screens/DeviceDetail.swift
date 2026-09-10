// The area on the right when a device is selected (docs/ia.md, On the
// Mac): Transfers, then Info, stacked as two sections of one view.

import SwiftUI

struct DeviceDetail: View {
    let device: DeviceInfo
    let transfers: [TransferInfo]
    var onForget: () -> Void = {}

    var body: some View {
        Form {
            Section(S.deviceDetail.transfersSection) {
                if transfers.isEmpty {
                    EmptyState(line: S.deviceDetail.noTransfers)
                } else {
                    ForEach(transfers) { transfer in
                        TransferRow(transfer: transfer)
                    }
                }
            }

            Section(S.deviceDetail.infoSection) {
                LabeledContent(S.deviceDetail.pairedLabel, value: FerryFormat.longDate(device.pairedDate))
                LabeledContent(S.deviceDetail.keyFingerprintLabel, value: device.keyFingerprint)
                    .font(FerryFont.mono)
                Button(S.deviceDetail.forgetThisPhone, role: .destructive, action: onForget)
            }
        }
        .formStyle(.grouped)
        .navigationTitle(device.name)
    }
}

/// A transfer as one row in the Transfers section: file name, a
/// ProgressLine, and the TransportBadge while it is active
/// (docs/ia.md, Transfers, Active).
private struct TransferRow: View {
    let transfer: TransferInfo

    var body: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s2) {
            HStack {
                Text(transfer.fileName)
                    .font(FerryFont.label)
                    .foregroundStyle(FerryColor.text)
                Spacer()
                if transfer.state == .active {
                    TransportBadge(state: TransportBadgeState(transfer: transfer))
                }
            }
            ProgressLine(transfer: transfer)
        }
        .padding(.vertical, FerrySpace.s1)
    }
}

#Preview {
    DeviceDetail(device: SampleState.reachablePhone, transfers: SampleState.transfers)
}

#Preview("No transfers") {
    DeviceDetail(device: SampleState.unreachablePhone, transfers: [])
}
