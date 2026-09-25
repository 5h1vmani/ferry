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

    /// How many rows show inline, above "Forget this phone", before a
    /// person has to open the full log. Chosen so the section stays
    /// shorter than the screen after one Finder browse of DCIM.
    /// `docs/audits/oss-looks.md`, M9.
    static let inlineLimit = 20

    @State private var showsFullLog = false

    private var totalEntries: Int {
        days.reduce(0) { $0 + $1.entries.count }
    }

    /// The first `inlineLimit` entries, grouped the same way as `days`, cut
    /// off mid-day rather than dropping a whole day early.
    private var inlineDays: [AccessDaySnapshot] {
        var remaining = Self.inlineLimit
        var result: [AccessDaySnapshot] = []
        for day in days {
            guard remaining > 0 else { break }
            let entries = Array(day.entries.prefix(remaining))
            result.append(AccessDaySnapshot(title: day.title, entries: entries))
            remaining -= entries.count
        }
        return result
    }

    var body: some View {
        Section(S.accessLog.section) {
            if days.isEmpty {
                EmptyState(line: S.accessLog.empty)
            } else {
                ForEach(inlineDays) { day in
                    Text(day.title)
                        .font(FerryFont.label)
                        .foregroundStyle(FerryColor.textSecondary)
                    ForEach(day.entries) { entry in
                        AccessLogRow(entry: entry, peerName: peerName)
                    }
                }
                if totalEntries > Self.inlineLimit {
                    Button(S.accessLog.seeAllAccess) {
                        showsFullLog = true
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
        .sheet(isPresented: $showsFullLog) {
            FullAccessLogSheet(days: days, peerName: peerName)
        }
    }
}

/// The whole access log for one device, in its own sheet. Reached from
/// `AccessLogSection` once there are more rows than the section shows
/// inline. `docs/audits/oss-looks.md`, M9.
private struct FullAccessLogSheet: View {
    let days: [AccessDaySnapshot]
    let peerName: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                ForEach(days) { day in
                    Section(day.title) {
                        ForEach(day.entries) { entry in
                            AccessLogRow(entry: entry, peerName: peerName)
                        }
                    }
                }
            }
            .navigationTitle(S.accessLog.fullLogTitle)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(S.accessLog.done) { dismiss() }
                }
            }
        }
        .frame(minWidth: 480, minHeight: 400)
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
