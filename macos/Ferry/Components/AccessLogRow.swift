// One file operation, as a sentence (docs/components.md,
// AccessLogRow). L5, job 9.
//
// The subject is always named — "Pixel 3 XL read" or "This Mac read" —
// never "you" and never a direction icon. A log is read months later, out
// of context, and an arrow does not survive that.
//
// Nothing in the row is coloured and nothing in it is a control. A row
// states a fact, offers no judgment, and offers no action.

import SwiftUI

struct AccessLogRow: View {
    let entry: AccessEntrySnapshot
    /// The peer's name, for the sentence's subject. The row does not know
    /// which device it belongs to; it is told.
    let peerName: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: FerrySpace.s3) {
            Text(entry.time)
                .font(FerryFont.mono)
                .foregroundStyle(FerryColor.textSecondary)
                .frame(width: 44, alignment: .leading)

            Text(sentence)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.text)
                .frame(maxWidth: .infinity, alignment: .leading)

            if let amount = entry.amount {
                Text(amount)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.textSecondary)
            }
        }
        .padding(.vertical, FerrySpace.s1)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            S.accessLog.accessibilityLabel(
                sentence: sentence,
                time: entry.time,
                amount: entry.amount
            )
        )
    }

    /// "Pixel 3 XL read Desktop/Q3 notes.md", built from the words in
    /// Strings.swift. The path keeps its own casing and is not translated.
    private var sentence: AttributedString {
        var subject = AttributedString(
            S.accessLog.subject(actor: entry.actor, peerName: peerName) + " "
        )
        subject.append(AttributedString(S.accessLog.verb(entry.verb) + " "))

        var path = AttributedString(entry.path)
        // The path is machine-produced, so it is set in mono inside a
        // sentence that is not (readme.md, Type: mono marks a
        // machine-produced value).
        path.font = FerryFont.mono
        subject.append(path)

        if let files = entry.files {
            subject.append(AttributedString(", " + S.transfers.fileCount(Int(files))))
        }
        return subject
    }
}

#if DEBUG
#Preview {
    VStack(alignment: .leading, spacing: 0) {
        ForEach(PreviewData.accessEntries) { entry in
            AccessLogRow(entry: entry, peerName: "Pixel 3 XL")
            Divider()
        }
    }
    .frame(width: 620)
    .padding()
}
#endif
