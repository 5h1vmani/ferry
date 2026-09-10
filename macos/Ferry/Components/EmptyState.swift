// One line and one action, centred (docs/components.md, EmptyState). Used
// by Devices with no devices, and by Transfers with no transfers, where
// there is no action.

import SwiftUI

struct EmptyState: View {
    let line: String
    var actionLabel: String?
    var action: (() -> Void)?

    var body: some View {
        VStack(spacing: FerrySpace.s4) {
            Text(line)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.textSecondary)

            if let actionLabel, let action {
                Button(actionLabel, action: action)
                    .buttonStyle(.borderedProminent)
                    .tint(FerryColor.accent)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(FerrySpace.s6)
    }
}

#Preview {
    VStack {
        EmptyState(line: S.devices.noPhonePaired, actionLabel: S.devices.pairAPhone, action: {})
        Divider()
        EmptyState(line: S.deviceDetail.noTransfers)
    }
}
