// One paired device in the Devices list (docs/components.md, DeviceRow).
// The platform row is a NavigationLink inside the sidebar List; this view
// is only the row's content.

import SwiftUI

struct DeviceRow: View {
    let device: DeviceInfo

    private var isReachable: Bool { device.reachableVia != nil }

    var body: some View {
        HStack(spacing: FerrySpace.s3) {
            Image(systemName: FerryIcon.devicePhone)
                .foregroundStyle(isReachable ? FerryColor.text : FerryColor.textSecondary)

            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                Text(device.name)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)

                TransportBadge(state: TransportBadgeState(device: device))

                if let lastSeen = device.lastSeen {
                    Text(S.devices.lastSeen(FerryFormat.relative(lastSeen)))
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    private var accessibilityLabel: String {
        let badgeLabel = TransportBadge.accessibilityText(for: TransportBadgeState(device: device))
        let lastSeenText = device.lastSeen.map { S.devices.lastSeen(FerryFormat.relative($0)) }
        return S.deviceRow.accessibilityLabel(name: device.name, badge: badgeLabel, lastSeen: lastSeenText)
    }
}

#Preview {
    List {
        DeviceRow(device: SampleState.reachablePhone)
        DeviceRow(device: SampleState.unreachablePhone)
    }
}
