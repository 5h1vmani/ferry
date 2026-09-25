// One paired device in the Devices list (docs/components.md, DeviceRow).
//
// Built from the platform sidebar row. The row states the device's name,
// how it is reachable, the spare transport when there is one, and when it
// was last seen when it is not reachable. It is one accessibility element,
// not four, so a screen reader reads it as one sentence.

import AppKit
import SwiftUI

struct DeviceRow: View {
    @EnvironmentObject private var model: EngineModel
    let device: DeviceSnapshot
    /// True while the Forget confirmation is up. `docs/audits/
    /// oss-looks.md`, M2: a context menu click no longer ends the pairing
    /// on its own.
    @State private var isConfirmingForget = false

    var body: some View {
        HStack(spacing: FerrySpace.s2) {
            Image(systemName: device.kind.icon)
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
            guard device.isReachable else {
                model.actionError = DropError.deviceNotReachable.threePart(canRetry: false)
                return false
            }
            model.send(urls: urls, toDevice: device.keyHex)
            return true
        }
        .contextMenu {
            if let path = model.mount(forDevice: device.keyHex).path {
                Button(S.access.openInFinder) {
                    NSWorkspace.shared.open(URL(fileURLWithPath: path))
                }
            }
            Button(S.devices.sendFiles) {
                SendFilesPanel.present(forDevice: device.keyHex, model: model)
            }
            Button(S.deviceDetail.forgetThisPhone, role: .destructive) {
                isConfirmingForget = true
            }
        }
        .confirmationDialog(
            S.deviceDetail.forgetConfirmTitle(deviceName: device.name),
            isPresented: $isConfirmingForget,
            titleVisibility: .visible
        ) {
            Button(S.deviceDetail.forgetThisPhone, role: .destructive) {
                model.forget(keyHex: device.keyHex)
            }
            Button(S.common.cancel, role: .cancel) {}
        } message: {
            Text(S.deviceDetail.forgetConfirmMessage)
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
