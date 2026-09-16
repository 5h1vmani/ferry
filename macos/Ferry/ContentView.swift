// One window: a sidebar listing Devices, and the selected device's detail
// on the right (docs/ia.md, On the Mac). No tabs, no toolbar clutter.
//
// Everything shown here comes from the engine, through the snapshots the
// model publishes. When the engine could not start, the window shows that
// error instead, with a Retry control.

import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: EngineModel
    @State private var isPairingPresented = false

    /// The selected device's public key, which is its identity. Held on
    /// the model, not here, so the app menu's commands can read it too
    /// (docs/ux-fix-plan.md, item 3, "Device choice").
    private var selection: Binding<String?> {
        Binding(
            get: { model.selectedDeviceKeyHex },
            set: { model.selectedDeviceKeyHex = $0 }
        )
    }

    var body: some View {
        Group {
            if let startError = model.startError {
                ErrorBlock(error: startError, onRetry: { Task { await model.start() } })
                    .padding(FerrySpace.s6)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            } else {
                window
            }
        }
        .sheet(isPresented: $isPairingPresented) {
            PairingSheet(selection: selection)
                .environmentObject(model)
        }
    }

    private var window: some View {
        NavigationSplitView {
            DevicesSidebar(
                devices: model.devices,
                presence: model.presence,
                selection: selection,
                onPair: { isPairingPresented = true },
                onAdvertisingChange: { model.setAdvertising($0) }
            )
        } detail: {
            if let device = selectedDevice {
                DeviceDetail(device: device)
            } else if model.devices.isEmpty {
                EmptyState(line: S.devices.noPhonePaired)
            } else {
                EmptyState(line: S.devices.noPhoneSelected)
            }
        }
        .onChange(of: model.devices.map(\.id)) { _, ids in
            if let current = model.selectedDeviceKeyHex, !ids.contains(current) {
                model.selectedDeviceKeyHex = ids.first
            } else if model.selectedDeviceKeyHex == nil {
                model.selectedDeviceKeyHex = ids.first
            }
        }
    }

    private var selectedDevice: DeviceSnapshot? {
        guard let keyHex = model.selectedDeviceKeyHex else { return nil }
        return model.device(keyHex: keyHex)
    }
}
