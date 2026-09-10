// A sheet over the window (docs/ia.md, On the Mac, Pairing). The engine
// drives every state; this sheet chooses which way in and shows whatever
// the engine reports.
//
// Two methods, one destination. A scan is fewer steps when both devices are
// in reach, which is when pairing happens, so it is offered first. The code
// method is the whole of the first version's flow and is one control away.
// Both end at Confirmed, with the same device in the same list, and every
// screen after pairing is untouched.
//
// Pairing is a once-ever job, so two ways in is one screen more, not two
// things to maintain forever.

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
        .frame(minWidth: 460, minHeight: 420)
        .onAppear {
            model.startPairing(method: .scan)
        }
        .onDisappear {
            // Closing the sheet any other way, such as with the Escape key,
            // leaves the engine pairing until it times out.
            if case .confirmed = model.pairing {
                return
            }
            model.cancelPairing()
        }
        .onChange(of: model.pairing) { _, screen in
            guard case let .confirmed(keyHex) = screen else { return }
            selection = keyHex
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
        case .choosing:
            // Reached only if the engine has not moved yet. Both controls
            // are shown rather than guessing which a person wants.
            VStack(spacing: FerrySpace.s3) {
                Button(S.pairing.scanTheCode) {
                    model.startPairing(method: .scan)
                }
                .buttonStyle(.borderedProminent)
                Button(S.pairing.useCodeInstead) {
                    model.startPairing(method: .code)
                }
                .buttonStyle(.bordered)
            }

        case let .offering(offer):
            PairingQRView(
                offer: offer,
                onUseCode: { model.startPairing(method: .code) },
                onCancel: cancel
            )

        case let .requested(name, transport):
            PairingRequestView(
                name: name,
                transport: transport,
                onPair: { model.confirmPairing(accept: true) },
                onRefuse: { model.confirmPairing(accept: false) }
            )

        case .waiting:
            VStack(spacing: FerrySpace.s3) {
                Text(S.pairing.waitingHeadline)
                    .font(FerryFont.title)
                    .foregroundStyle(FerryColor.text)
                Text(S.pairing.waitingBody)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.textSecondary)
                    .multilineTextAlignment(.center)
                Button(S.common.cancel, action: cancel)
                    .buttonStyle(.bordered)
            }

        case let .found(candidates):
            VStack(alignment: .leading, spacing: FerrySpace.s2) {
                ForEach(candidates) { candidate in
                    Button {
                        model.pickCandidate(id: candidate.id)
                    } label: {
                        Text(candidate.label)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.bordered)
                }
            }
            .frame(maxWidth: 320)

        case let .code(digits):
            PairingCode(
                state: .showing(code: digits),
                onConfirm: { model.confirmPairing(accept: true) },
                onCancel: cancel
            )

        case .confirmed:
            PairingCode(state: .confirmed)

        case let .failed(error):
            PairingCode(state: .mismatched(error), onRetry: {
                model.startPairing(method: .scan)
            })
        }
    }

    private func cancel() {
        model.cancelPairing()
        dismiss()
    }
}
