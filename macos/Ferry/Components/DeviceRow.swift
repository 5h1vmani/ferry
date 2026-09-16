// One paired device in the Devices list (docs/components.md, DeviceRow).
//
// Built from the platform sidebar row. The row states the device's name,
// how it is reachable, the spare transport when there is one, and when it
// was last seen when it is not reachable. It is one accessibility element,
// not four, so a screen reader reads it as one sentence.

import SwiftUI

struct DeviceRow: View {
    @EnvironmentObject private var model: EngineModel
    let device: DeviceSnapshot

    var body: some View {
        HStack(spacing: FerrySpace.s2) {
            Image(systemName: icon)
                .foregroundStyle(device.isReachable ? FerryColor.text : FerryColor.textSecondary)

            VStack(alignment: .leading, spacing: 1) {
                Text(device.name)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)
                    .lineLimit(1)

                TransportBadge(state: device.badge)

                if let spare = device.spareTransport {
                    Text(S.devices.spareTransport(spare))
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }

                if let lastSeen = device.lastSeen {
                    Text(lastSeen)
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }
            }
        }
        .frame(minHeight: 32)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            S.deviceRow.accessibilityLabel(
                name: device.name,
                badge: TransportBadge.accessibilityText(for: device.badge),
                spareTransport: device.spareTransport.map { S.devices.spareTransport($0) },
                lastSeen: device.lastSeen
            )
        )
        .dropDestination(for: URL.self) { urls, _ in
            guard device.isReachable else { return false }
            model.send(urls: urls, toDevice: device.keyHex)
            return true
        }
        .contextMenu {
            Button(S.devices.sendFiles) {
                SendFilesPanel.present(forDevice: device.keyHex, model: model)
            }
        }
    }

    private var icon: String {
        switch device.kind {
        case .phone: return FerryIcon.devicePhone
        case .mac: return FerryIcon.deviceMac
        }
    }
}

#if DEBUG
#Preview {
    List {
        ForEach(PreviewData.deviceSnapshots) { device in
            DeviceRow(device: device)
        }
    }
    .frame(width: 232)
    .environmentObject(EngineModel())
}
#endif
