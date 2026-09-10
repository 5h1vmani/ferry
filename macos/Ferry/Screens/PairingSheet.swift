// A sheet over the window (docs/ia.md, On the Mac, Pairing). The engine
// drives every state: the sheet asks it to start pairing when it opens, and
// then shows whatever the engine reports.
//
// Confirmed closes the sheet and selects the new device, which is the last
// step of the Mac's first run.

import SwiftUI

struct PairingSheet: View {
    @EnvironmentObject private var model: EngineModel
    @Environment(\.dismiss) private var dismiss
    /// The window's selected device. Pairing sets it when it succeeds.
    @Binding var selection: String?

    var body: some View {
        VStack(spacing: FerrySpace.s5) {
            Text(S.pairing.title)
                .font(FerryFont.title)
                .foregroundStyle(FerryColor.text)

            Spacer(minLength: 0)
            content
            Spacer(minLength: 0)
        }
        .padding(FerrySpace.s6)
        .frame(minWidth: 460, minHeight: 380)
        .onAppear {
            model.startPairing()
        }
        .onDisappear {
            // Closing the sheet any other way, such as with the Escape key,
            // leaves the engine pairing until it times out.
            if case .confirmed = model.pairing {
                return
            }
            model.cancelPairing()
        }
        .onChange(of: model.pairing) { _, state in
            guard case .confirmed(let device) = state else { return }
            selection = device.keyHex
            Task {
                // The paired icon shows for one second, then the sheet
                // closes (docs/components.md, PairingCode).
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                dismiss()
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.pairing {
        case .idle, .waiting:
            VStack(spacing: FerrySpace.s3) {
                Text(S.pairing.waitingHeadline)
                    .font(FerryFont.title)
                    .foregroundStyle(FerryColor.text)
                Text(S.pairing.waitingBody)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.textSecondary)
                    .multilineTextAlignment(.center)
                Button(S.common.cancel) {
                    model.cancelPairing()
                    dismiss()
                }
                .buttonStyle(.bordered)
            }

        case .found(let candidates):
            VStack(alignment: .leading, spacing: FerrySpace.s2) {
                ForEach(candidates) { candidate in
                    Button {
                        model.pickCandidate(id: candidate.id)
                    } label: {
                        Text(PairingSheet.label(for: candidate))
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.bordered)
                }
            }
            .frame(maxWidth: 320)

        case .code(let code):
            PairingCode(
                state: .showing(code: FerryFormat.pairingCode(code)),
                onConfirm: { model.confirmPairing(accept: true) },
                onCancel: {
                    model.cancelPairing()
                    dismiss()
                }
            )

        case .confirmed:
            PairingCode(state: .confirmed)

        case .failed(let error):
            PairingCode(
                state: .mismatched(ThreePartError(error, canRetry: true)),
                onRetry: { model.startPairing() }
            )
        }
    }

    /// "Phone over USB" or "Phone on Wi-Fi · 3F9A" (docs/ia.md, Pairing,
    /// Found).
    private static func label(for candidate: PairingCandidate) -> String {
        switch candidate.transport {
        case .usb:
            return S.pairing.candidateUSB
        case .wifi:
            return S.pairing.candidateWifi(shortCode: candidate.shortCode)
        }
    }
}
