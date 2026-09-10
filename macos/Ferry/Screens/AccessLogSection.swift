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
import AppKit

struct AccessLogSection: View {
    let days: [AccessDaySnapshot]
    let peerName: String
    /// Where the engine keeps the log, so a person can reach the file
    /// itself. Nil until the engine writes one.
    var logPath: String?

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

            HStack(spacing: FerrySpace.s3) {
                // Stated because a log that quietly forgets is worse than
                // no log.
                Text(S.accessLog.retention)
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
                Spacer()
                if let logPath {
                    Button(S.accessLog.revealInFinder) {
                        NSWorkspace.shared.selectFile(logPath, inFileViewerRootedAtPath: "")
                    }
                }
            }
        }
    }
}

#if DEBUG
#Preview {
    Form {
        AccessLogSection(days: PreviewData.accessDays, peerName: "Pixel 3 XL")
    }
    .formStyle(.grouped)
    .frame(width: 700, height: 420)
}

#Preview("Empty") {
    Form {
        AccessLogSection(days: [], peerName: "Pixel 3 XL")
    }
    .formStyle(.grouped)
    .frame(width: 700, height: 240)
}
#endif
