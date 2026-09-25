// The three-part rule from docs/voice.md, as a view (docs/components.md,
// ErrorBlock): what stopped, why, what to do. Any part that is unknown is
// left out by whoever builds the ThreePartError, never guessed here.
// Never red; red is for danger, and nothing Ferry shows here is dangerous.

import SwiftUI

struct ErrorBlock: View {
    let error: ThreePartError
    var onRetry: () -> Void = {}

    var body: some View {
        HStack(alignment: .top, spacing: FerrySpace.s3) {
            Image(systemName: FerryIcon.failed)
                .foregroundStyle(FerryColor.text)

            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                VStack(alignment: .leading, spacing: FerrySpace.s1) {
                    Text(error.whatStopped)
                        .font(FerryFont.body.bold())
                        .foregroundStyle(FerryColor.text)
                    Text(error.why)
                        .font(FerryFont.body)
                        .foregroundStyle(FerryColor.textSecondary)
                    Text(error.whatToDo)
                        .font(FerryFont.body)
                        .foregroundStyle(FerryColor.text)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(
                    S.errorBlock.accessibilityLabel(
                        whatStopped: error.whatStopped,
                        why: error.why,
                        whatToDo: error.whatToDo
                    )
                )

                if error.canRetry {
                    Button(S.common.retry, action: onRetry)
                        .buttonStyle(.bordered)
                        .tint(FerryColor.accentText)
                }
            }
        }
        .padding(FerrySpace.s3)
        .background(FerryColor.surfaceRaised)
        .clipShape(RoundedRectangle(cornerRadius: FerryRadius.medium))
    }
}

#Preview {
    ErrorBlock(
        error: ThreePartError(
            whatStopped: "Chunk 14 of IMG_0410.jpg failed to verify.",
            why: "The file changed on the phone during transfer.",
            whatToDo: "Send it again.",
            canRetry: true
        )
    )
    .padding()
}
