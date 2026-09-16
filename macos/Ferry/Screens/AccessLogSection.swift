// The access log, as a section of the selected device (docs/ia.md, The
// access log). L5, job 9.
//
// One list per paired device, holding what this Mac served to that phone
// and what it read from it. Grouped by day, newest first, one row per file
// operation.
//
// Nothing here notifies, badges, or judges. It records what happened and
// does not decide that something was wrong. See docs/jobs.md, what job 9
// is not.

import SwiftUI

struct AccessLogSection: View {
    let days: [AccessDaySnapshot]
    let peerName: String
    /// From the model's `accessLogRetentionDays`. Nil until the engine has
    /// started, in which case the retention line is left out rather than
    /// guessed.
    let retentionDays: UInt32?

    var body: some View {
        Section(S.accessLog.section) {
            if days.isEmpty {
                EmptyState(line: S.accessLog.empty)
            } else {
                ForEach(days) { day in
                    Text(day.title)
                        .font(FerryFont.label)
                        .foregroundStyle(FerryColor.textSecondary)
                    ForEach(day.entries) { entry in
                        AccessLogRow(entry: entry, peerName: peerName)
                    }
                }
            }

            // Stated because a log that quietly forgets is worse than no
            // log.
            if let retentionDays {
                Text(S.accessLog.retention(days: retentionDays))
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
            }
        }
    }
}

#if DEBUG
#Preview {
    Form {
        AccessLogSection(days: PreviewData.accessDays, peerName: "Pixel 3 XL", retentionDays: 30)
    }
    .formStyle(.grouped)
    .frame(width: 700, height: 420)
}

#Preview("Empty") {
    Form {
        AccessLogSection(days: [], peerName: "Pixel 3 XL", retentionDays: 30)
    }
    .formStyle(.grouped)
    .frame(width: 700, height: 240)
}
#endif
