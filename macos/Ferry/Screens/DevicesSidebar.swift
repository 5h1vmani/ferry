// The sidebar on the left: one row per paired device, and "Pair a phone"
// at the bottom (docs/ia.md, On the Mac). Empty shows EmptyState instead.

import SwiftUI

struct DevicesSidebar: View {
    let devices: [DeviceInfo]
    @Binding var selection: DeviceInfo.ID?
    var onPair: () -> Void = {}

    var body: some View {
        Group {
            if devices.isEmpty {
                EmptyState(line: S.devices.noPhonePaired, actionLabel: S.devices.pairAPhone, action: onPair)
            } else {
                VStack(spacing: 0) {
                    List(devices, selection: $selection) { device in
                        DeviceRow(device: device)
                            .tag(device.id)
                    }
                    Divider()
                    Button(action: onPair) {
                        Label(S.devices.pairAPhone, systemImage: FerryIcon.pair)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.plain)
                    .padding(FerrySpace.s3)
                }
            }
        }
        .navigationTitle(S.devices.sidebarTitle)
    }
}

#Preview {
    NavigationSplitView {
        DevicesSidebar(devices: SampleState.devices, selection: .constant(nil))
    } detail: {
        EmptyState(line: S.devices.noPhoneSelected)
    }
}

#Preview("Empty") {
    NavigationSplitView {
        DevicesSidebar(devices: [], selection: .constant(nil))
    } detail: {
        EmptyState(line: S.devices.noPhoneSelected)
    }
}
