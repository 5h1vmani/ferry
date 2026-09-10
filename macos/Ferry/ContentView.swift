// One window: a sidebar listing Devices, and the selected device's detail
// on the right (docs/ia.md, On the Mac). No tabs, no toolbar clutter.
//
// Everything shown here comes from the engine, through the snapshots the
// model publishes. When the engine could not start, the window shows that
// error instead, with a Retry control.

import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: EngineModel
    /// The selected device's public key, which is its identity.
    @State private var selection: String?
    @State private var isPairingPresented = false

    var body: some View {
        Group {
            if let startError = model.startError {
                ErrorBlock(error: startError, onRetry: { model.start() })
                    .padding(FerrySpace.s6)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            } else {
                window
            }
        }
        .sheet(isPresented: $isPairingPresented) {
            PairingSheet(selection: $selection)
                .environmentObject(model)
        }
    }

    private var window: some View {
        NavigationSplitView {
            DevicesSidebar(
                devices: model.devices,
                presence: model.presence,
                selection: $selection,
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
            if let selection, !ids.contains(selection) {
                self.selection = ids.first
            } else if selection == nil {
                selection = ids.first
            }
        }
    }

    private var selectedDevice: DeviceSnapshot? {
        guard let selection else { return nil }
        return model.device(keyHex: selection)
    }
}
