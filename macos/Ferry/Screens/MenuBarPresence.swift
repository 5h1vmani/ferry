// Presence in the menu bar (docs/ia.md, L0).
//
// The Mac's equivalent of the phone's persistent notification: the honest
// answer to "is Ferry running", available without opening anything. Job 2
// is finished by plugging in a cable, and a person who has just done that
// looks for confirmation, not for a window.
//
// One rule keeps this from becoming a second window: it shows state and the
// privacy mode, never history and never a transfer list. The moment it
// needs a scroll view, it has become the thing it was built to avoid.

import SwiftUI
import AppKit

struct MenuBarPresence: View {
    @EnvironmentObject private var model: EngineModel

    var body: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s2) {
            if model.devices.isEmpty {
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

            Divider()

            PresenceControl(
                presence: model.presence,
                onChange: { model.setAdvertising($0) },
                showsSpeed: true
            )
            .padding(.horizontal, FerrySpace.s2)

            Divider()

            Button(S.menuBar.openFerry) {
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
