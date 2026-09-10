// The six digits, on both devices at once (docs/components.md,
// PairingCode). The code is never in a text field; it is compared by eye.

import SwiftUI

enum PairingCodeState: Equatable {
    case showing(code: String)
    case confirmed
    case mismatched(ThreePartError)
}

struct PairingCode: View {
    let state: PairingCodeState
    var onConfirm: () -> Void = {}
    var onCancel: () -> Void = {}
    var onRetry: () -> Void = {}

    var body: some View {
        switch state {
        case .showing(let code):
            VStack(spacing: FerrySpace.s4) {
                codeText(code)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(S.pairing.codeAccessibility(FerryFormat.spokenDigits(code)))

                Text(S.pairing.codeInstruction)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)

                HStack(spacing: FerrySpace.s3) {
                    Button(S.common.cancel, action: onCancel)
                        .buttonStyle(.bordered)
                    Button(S.common.confirm, action: onConfirm)
                        .buttonStyle(.borderedProminent)
                        .tint(FerryColor.accent)
                }
            }

        case .confirmed:
            VStack(spacing: FerrySpace.s3) {
                Image(systemName: FerryIcon.paired)
                    .font(.largeTitle)
                    .foregroundStyle(FerryColor.accent)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(S.pairing.accessibilityConfirmed)

        case .mismatched(let error):
            ErrorBlock(error: error, onRetry: onRetry)
        }
    }

    /// "481 920", grouped three and three with a space.3 gap, in the
    /// display type with monospaced digits.
    private func codeText(_ code: String) -> some View {
        let groups = code.split(separator: " ").map(String.init)
        return HStack(spacing: FerrySpace.s3) {
            ForEach(groups, id: \.self) { group in
                Text(group)
                    .font(FerryFont.display)
                    .monospacedDigit()
                    .foregroundStyle(FerryColor.text)
            }
        }
    }
}

#Preview {
    VStack(spacing: FerrySpace.s6) {
        PairingCode(state: .showing(code: "481 920"))
        PairingCode(state: .confirmed)
        PairingCode(state: .mismatched(ThreePartError(
            whatStopped: "Pairing stopped.",
            why: "The codes did not match.",
            whatToDo: "Try again.",
            canRetry: true
        )))
    }
    .padding()
}
