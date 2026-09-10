// One paired device in the Devices list (docs/components.md, DeviceRow).
// The platform row is a row inside the sidebar List; this view is only the
// row's content.

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

                if let lastSeen = lastSeenText {
                    Text(lastSeen)
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    /// Only shown while the device is not reachable, which is when the
    /// engine reports a last seen time.
    private var lastSeenText: String? {
        guard let seconds = device.lastSeenUnixSecs else { return nil }
        return S.devices.lastSeen(FerryFormat.relative(unixSecs: seconds))
    }

    private var accessibilityLabel: String {
        let badgeLabel = TransportBadge.accessibilityText(for: TransportBadgeState(device: device))
        return S.deviceRow.accessibilityLabel(
            name: device.name,
            badge: badgeLabel,
            lastSeen: lastSeenText
        )
    }
}

#if DEBUG
#Preview {
    List {
        DeviceRow(device: PreviewData.reachablePhone)
        DeviceRow(device: PreviewData.unreachablePhone)
    }
}
#endif
