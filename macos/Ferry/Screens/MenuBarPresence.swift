// Presence in the menu bar (docs/ia.md, L0).
//
// The Mac's equivalent of the phone's persistent notification: the honest
// answer to "is Ferry running", available without opening anything. Job 2
// is finished by plugging in a cable, and a person who has just done that
// looks for confirmation, not for a window.
//
// One rule keeps this from becoming a second window: it shows state, the
// privacy mode, and one line per batch moving right now: label, progress,
// speed. It never shows a finished transfer or the access log
// (docs/ux-fix-plan.md, item 2). The moment it needs a scroll view, it has
// become the thing it was built to avoid.

import SwiftUI
import AppKit

struct MenuBarPresence: View {
    @EnvironmentObject private var model: EngineModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s2) {
            if !model.isReady {
                Text(S.devices.starting)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.textSecondary)
                    .padding(.horizontal, FerrySpace.s2)
            } else if model.devices.isEmpty {
                Text(S.devices.noPhonePaired)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.textSecondary)
                    .padding(.horizontal, FerrySpace.s2)
            } else {
                ForEach(model.devices) { device in
                    DeviceRow(device: device)
                        .padding(.horizontal, FerrySpace.s2)
                }
            }

            if !model.runningBatches.isEmpty {
                Divider()
                ForEach(model.runningBatches) { group in
                    RunningBatchLine(group: group)
                        .padding(.horizontal, FerrySpace.s2)
                }
            }

            Divider()

            PresenceControl(
                presence: model.presence,
                onChange: { model.setAdvertising($0) },
                showsSpeed: true,
                isReady: model.isReady
            )
            .padding(.horizontal, FerrySpace.s2)

            Divider()

            Button(S.menuBar.openFerry) {
                // `activate` alone brings the app forward but creates no
                // window, so a closed window stayed closed.
                // `docs/audits/oss-looks.md`, M4.
                openWindow(id: mainWindowID)
                NSApplication.shared.activate(ignoringOtherApps: true)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, FerrySpace.s2)
        }
        .padding(.vertical, FerrySpace.s2)
        .frame(width: 260)
    }
}

/// What sits in the menu bar itself: the transport icon and, while bytes
/// are moving, the speed. Nothing else. A speed is the one number worth a
/// permanent place on screen, because it is the one a person is waiting on.
struct MenuBarLabel: View {
    let presence: PresenceSnapshot

    var body: some View {
        HStack(spacing: FerrySpace.s1) {
            if !presence.isAdvertising {
                Image(systemName: FerryIcon.advertisingOff)
            }
            if let transport = presence.activeTransport {
                Image(systemName: transport == .usb ? FerryIcon.usb : FerryIcon.wifi)
            }
            if let speed = presence.speedBytesPerSec, speed > 0 {
                Text(FerryFormat.speed(bytesPerSec: speed))
                    .font(FerryFont.mono)
            }
            // With nothing moving and advertising on, the mark alone is
            // the whole message: Ferry is running.
            if presence.isAdvertising && presence.activeTransport == nil {
                Image(systemName: FerryIcon.transfer)
            }
        }
        .accessibilityLabel(S.menuBar.accessibilityLabel)
    }
}

/// One running batch's line in the menu bar dropdown: its label, a
/// progress bar, and its speed. `docs/ux-fix-plan.md`, item 2.
private struct RunningBatchLine: View {
    let group: TransferGroupSnapshot

    var body: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s1) {
            Text(group.label)
                .font(FerryFont.label)
                .foregroundStyle(FerryColor.text)
                .lineLimit(1)
            HStack(spacing: FerrySpace.s2) {
                ProgressView(value: group.fraction)
                    .tint(FerryColor.accent)
                if let speed = group.speedBytesPerSec, speed > 0 {
                    Text(FerryFormat.speed(bytesPerSec: speed))
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            [
                group.label,
                S.progressLine.accessibilityTransferring(percent: group.percent),
            ].joined(separator: ". ")
        )
    }
}
