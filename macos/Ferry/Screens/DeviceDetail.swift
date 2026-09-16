// The area on the right when a device is selected (docs/ia.md, On the
// Mac). Five sections, in the order a person needs them:
//
//   Access      the Finder mount, one line, absent until it is ready   L2
//   Automatic   copy new photos, one switch                           L3
//   Transfers   what is moving, newest first                          L3
//   Access log  what this pair actually did                           L5
//   Info        four facts and Forget, as a footer                    L1
//
// Info was an equal section in the first version of the IA. It is four
// facts read twice a year, stacked against things that change every second,
// so it is demoted to a footer and stops competing.
//
// The Files section that used to sit here is removed
// (docs/decisions/0011-gestures-not-a-file-manager.md). A drop, "Send
// files…", and the Finder mount cover what it did. A caption at the top
// states where a drop lands, while the device is reachable.
//
// Five sections is the most this shape will carry. A sixth section would
// turn the pane into a list of destinations. That is a bigger change than
// adding a section, so a sixth has to earn it.

import SwiftUI

struct DeviceDetail: View {
    @EnvironmentObject private var model: EngineModel
    let device: DeviceSnapshot

    @State private var landingFolder: String?

    private var groups: [TransferGroupSnapshot] {
        model.groups(forDevice: device.keyHex)
    }

    var body: some View {
        Form {
            if device.isReachable, let landingFolder {
                Text(S.drop.caption(deviceName: device.name, folder: landingFolder))
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
            }

            if let actionError = model.actionError {
                ErrorBlock(error: actionError)
            }

            AccessSection(mount: model.mount(forDevice: device.keyHex))

            AutomaticSection(
                autoCopy: model.autoCopy(forDevice: device.keyHex),
                deviceName: device.name
            ) { enabled in
                model.setAutoCopy(forDevice: device.keyHex, enabled: enabled)
            }

            Section(S.deviceDetail.transfersSection) {
                if groups.isEmpty {
                    EmptyState(line: S.deviceDetail.noTransfers)
                } else {
                    ForEach(groups) { group in
                        TransferRow(group: group) {
                            switch group.retryTarget {
                            case .transfer(let id): model.retry(transferId: id)
                            case .batch(let id): model.retryBatch(batchId: id)
                            }
                        }
                    }
                }
            }

            AccessLogSection(
                days: model.accessLog(forDevice: device.keyHex),
                peerName: device.name
            )

            infoFooter
        }
        .formStyle(.grouped)
        .navigationTitle(device.name)
        .navigationSubtitle(subtitle)
        .task(id: landingFolderReloadKey) {
            await loadLandingFolder()
        }
        .onChange(of: device.keyHex) { _, _ in
            model.actionError = nil
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard device.isReachable else { return false }
            model.send(urls: urls, toDevice: device.keyHex)
            return true
        }
    }

    /// The active transport, and the spare stated once beside it, so a
    /// pulled cable is not a surprise (docs/ia.md, Devices).
    private var subtitle: String {
        var parts = [TransportBadge.accessibilityText(for: device.badge)]
        if let spare = device.spareTransport {
            parts.append(S.devices.spareTransport(spare))
        }
        return parts.joined(separator: S.common.dotSeparator)
    }

    /// Changes whenever the device switches, or flips reachable, so the
    /// caption refetches instead of keeping a stale answer from before the
    /// device was reachable.
    private var landingFolderReloadKey: String {
        device.keyHex + "\u{0000}" + String(device.isReachable)
    }

    /// Four facts and one destructive control, in a footer rather than a
    /// section of its own.
    private var infoFooter: some View {
        Section {
            LabeledContent(S.deviceDetail.pairedLabel, value: device.pairedDate)
            LabeledContent(S.deviceDetail.keyFingerprintLabel) {
                Text(device.keyHex)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.textSecondary)
                    .textSelection(.enabled)
            }
            Button(S.deviceDetail.forgetThisPhone, role: .destructive) {
                model.forget(keyHex: device.keyHex)
            }
        }
    }

    private func loadLandingFolder() async {
        guard device.isReachable else {
            landingFolder = nil
            return
        }
        landingFolder = try? await model.landingFolder(forDevice: device.keyHex)
    }
}
