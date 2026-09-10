// A sheet over the window (docs/ia.md, On the Mac). No engine drives it
// yet, so a picker switches among the five states docs/ia.md lists, for
// viewing each one. The picker is scaffold only; it is not part of a real
// pairing flow, where the state changes on its own.

import SwiftUI

struct PairingSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State private var selectedIndex = 0

    private var current: PairingState {
        SampleState.allPairingStates[selectedIndex].state
    }

    var body: some View {
        VStack(spacing: FerrySpace.s5) {
            Text(S.pairing.title)
                .font(FerryFont.title)
                .foregroundStyle(FerryColor.text)

            Picker(S.debug.pairingStatePicker, selection: $selectedIndex) {
                ForEach(SampleState.allPairingStates.indices, id: \.self) { index in
                    Text(SampleState.allPairingStates[index].label).tag(index)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            Spacer(minLength: 0)
            content
            Spacer(minLength: 0)
        }
        .padding(FerrySpace.s6)
        .frame(minWidth: 460, minHeight: 380)
    }

    @ViewBuilder
    private var content: some View {
        switch current {
        case .waiting:
            VStack(spacing: FerrySpace.s3) {
                Text(S.pairing.waitingHeadline)
                    .font(FerryFont.title)
                    .foregroundStyle(FerryColor.text)
                Text(S.pairing.waitingBody)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.textSecondary)
                Button(S.common.cancel) { dismiss() }
                    .buttonStyle(.bordered)
            }

        case .found(let candidates):
            VStack(alignment: .leading, spacing: FerrySpace.s2) {
                ForEach(candidates) { candidate in
                    Button {
                        // Picking a candidate moves to Code once the engine drives this.
                    } label: {
                        Text(candidate.label)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.bordered)
                }
            }
            .frame(maxWidth: 320)

        case .code(let code):
            PairingCode(
                state: .showing(code: code),
                onConfirm: {},
                onCancel: { dismiss() }
            )

        case .confirmed:
            PairingCode(state: .confirmed)

        case .failed(let error):
            PairingCode(state: .mismatched(error), onRetry: {})
        }
    }
}

#Preview {
    PairingSheet()
}
