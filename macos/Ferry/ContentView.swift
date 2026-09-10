// One window: a sidebar listing Devices, and the selected device's detail
// on the right (docs/ia.md, On the Mac). No tabs, no toolbar clutter.

import SwiftUI

struct ContentView: View {
    @State private var selection: DeviceInfo.ID?
    @State private var isPairingPresented = false

    #if DEBUG
    /// Scaffold only: lets both the empty and the populated screen state
    /// be viewed without a running engine. Not part of the shipped app.
    @State private var sampleDataPopulated = true
    #endif

    private var devices: [DeviceInfo] {
        #if DEBUG
        sampleDataPopulated ? SampleState.devices : []
        #else
        SampleState.devices
        #endif
    }

    private func transfers(for deviceID: DeviceInfo.ID) -> [TransferInfo] {
        // Only the sample's reachable phone has sample transfers.
        deviceID == SampleState.reachablePhone.id ? SampleState.transfers : []
    }

    var body: some View {
        NavigationSplitView {
            DevicesSidebar(devices: devices, selection: $selection) {
                isPairingPresented = true
            }
        } detail: {
            if let selection, let device = devices.first(where: { $0.id == selection }) {
                DeviceDetail(device: device, transfers: transfers(for: device.id))
            } else if devices.isEmpty {
                EmptyState(line: S.devices.noPhonePaired)
            } else {
                EmptyState(line: S.devices.noPhoneSelected)
            }
        }
        .toolbar {
            ToolbarItem {
                Button {
                    isPairingPresented = true
                } label: {
                    Label(S.devices.pairAPhone, systemImage: FerryIcon.pair)
                }
                .accessibilityLabel(S.devices.pairAPhone)
            }

            #if DEBUG
            ToolbarItem {
                Menu(S.debug.sampleStateMenu) {
                    Button(S.debug.sampleStateEmpty) { sampleDataPopulated = false }
                    Button(S.debug.sampleStatePopulated) { sampleDataPopulated = true }
                }
            }
            #endif
        }
        .sheet(isPresented: $isPairingPresented) {
            PairingSheet()
        }
        .onChange(of: devices.map(\.id)) {
            if let selection, !devices.contains(where: { $0.id == selection }) {
                self.selection = devices.first?.id
            } else if selection == nil {
                selection = devices.first?.id
            }
        }
        .onAppear {
            if selection == nil {
                selection = devices.first?.id
            }
        }
    }
}

#Preview {
    ContentView()
}
