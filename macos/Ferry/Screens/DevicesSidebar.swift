// The sidebar on the left: one row per paired device, the presence control,
// and "Pair a phone" (docs/ia.md, On the Mac).
//
// The presence control is pinned to the footer rather than filed in
// Settings. It is a mode with a consequence, not a preference, and it is
// the one true fact Ferry can state about itself before anything is paired
// — so it shows in the empty state too.

import SwiftUI

struct DevicesSidebar: View {
    let devices: [DeviceSnapshot]
    let presence: PresenceSnapshot
    /// The selected device's public key.
    @Binding var selection: String?
    var onPair: () -> Void = {}
    var onAdvertisingChange: (Bool) -> Void = { _ in }

    var body: some View {
        VStack(spacing: 0) {
            if devices.isEmpty {
                EmptyState(line: S.devices.noPhonePaired, actionLabel: S.devices.pairAPhone, action: onPair)
                    .frame(maxHeight: .infinity)
            } else {
                List(devices, selection: $selection) { device in
                    DeviceRow(device: device)
                        .tag(device.keyHex)
                }
            }

            Divider()

            PresenceControl(presence: presence, onChange: onAdvertisingChange)
                .padding(.horizontal, FerrySpace.s3)
                .padding(.vertical, FerrySpace.s2)

            Divider()

            Button(action: onPair) {
                Label(S.devices.pairAPhone, systemImage: FerryIcon.pair)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            .padding(FerrySpace.s3)
        }
        .navigationTitle(S.devices.sidebarTitle)
    }
}

#if DEBUG
#Preview {
    NavigationSplitView {
        DevicesSidebar(
            devices: PreviewData.deviceSnapshots,
            presence: PreviewData.advertising,
            selection: .constant(nil)
        )
    } detail: {
        EmptyState(line: S.devices.noPhoneSelected)
    }
}

#Preview("Empty, not advertising") {
    NavigationSplitView {
        DevicesSidebar(
            devices: [],
            presence: .unknown,
            selection: .constant(nil)
        )
    } detail: {
        EmptyState(line: S.devices.noPhoneSelected)
    }
}
#endif
