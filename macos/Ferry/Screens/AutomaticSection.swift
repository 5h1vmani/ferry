// Job 7, which had no screen until this pass (docs/ia.md, Automatic).
//
// "New photos reach the Mac on their own." Nothing turned it on, nothing
// said it had happened, and nothing distinguished a copy Ferry decided to
// make from one a person asked for. This section is the first two; the
// third is the "Automatic" word on the Transfers row.
//
// One switch, per device. Not a global preference: a work phone and a
// personal phone are different answers, and the device's own screen is
// where that answer belongs.

import SwiftUI

struct AutomaticSection: View {
    let autoCopy: AutoCopySnapshot
    let deviceName: String
    let onChange: (Bool) -> Void

    var body: some View {
        Section(S.automatic.section) {
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                Toggle(S.automatic.copyNewPhotos(from: deviceName), isOn: binding)
                    // TODO(engine 14): no auto copy exists, so the switch
                    // is disabled rather than shown as off-but-available,
                    // which would be a small lie about what Ferry can do.
                    .disabled(!autoCopy.isSupported)

                // The rule is stated, not implied. This is the sentence
                // that separates job 7 from the two-way sync Ferry
                // refuses, and the one place a person could reasonably
                // fear the wrong thing.
                Text(S.automatic.rule(source: autoCopy.source, destination: destination))
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)

                // A count and a time. Not "up to date", which is an
                // adjective standing in for a number, and not a tick.
                if let lastRun = autoCopy.lastRun {
                    Text(lastRun)
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }
            }
        }
    }

    private var binding: Binding<Bool> {
        Binding(get: { autoCopy.isEnabled }, set: { onChange($0) })
    }

    /// The last two path components, so the line reads "Downloads/Ferry"
    /// rather than a full home path a person does not need here.
    private var destination: String {
        let parts = (autoCopy.destination as NSString).pathComponents
        return parts.suffix(2).joined(separator: "/")
    }
}

#if DEBUG
#Preview {
    Form {
        AutomaticSection(
            autoCopy: PreviewData.autoCopyOn,
            deviceName: "Pixel 3 XL",
            onChange: { _ in }
        )
        AutomaticSection(
            autoCopy: .unsupported,
            deviceName: "Pixel 3 XL",
            onChange: { _ in }
        )
    }
    .formStyle(.grouped)
    .frame(width: 700, height: 320)
}
#endif
