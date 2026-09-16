// Plain number formatting shared by the components. Ferry states numbers
// instead of adjectives (docs/voice.md, rule 5), so these helpers turn the
// raw bytes and Unix seconds the engine reports into the exact shapes the
// components print, such as "38 MB/s" or "2.1 GB".
//
// Every number a screen shows passes through this file. That is the point:
// two views that print a byte count must print it the same way, and a
// change to how Ferry rounds is a change in one place.

import Foundation

enum FerryFormat {
    /// "38 MB/s", for the visible label. Decimal megabytes, rounded.
    static func speed(bytesPerSec: UInt64) -> String {
        let mb = Double(bytesPerSec) / 1_000_000
        return "\(Int(mb.rounded())) MB/s"
    }

    /// "38 megabytes per second", for VoiceOver. Same value, spoken out.
    static func speedSpoken(bytesPerSec: UInt64) -> String {
        let mb = Double(bytesPerSec) / 1_000_000
        return "\(Int(mb.rounded())) megabytes per second"
    }

    /// "2.1 GB" for a byte count of a gigabyte or more, "120 MB" for a
    /// megabyte or more, and "4096 bytes" below that.
    static func bytes(_ count: UInt64) -> String {
        let value = Double(count)
        if value >= 1_000_000_000 {
            return String(format: "%.1f GB", value / 1_000_000_000)
        }
        if value >= 1_000_000 {
            return "\(Int((value / 1_000_000).rounded())) MB"
        }
        return "\(count) bytes"
    }

    /// "3 min" for how long a finished transfer took, "18 s" below a
    /// minute, and "1 h 12 min" above an hour. Used by a done row, once
    /// the engine carries timestamps (docs/engine-contract.md, item 9).
    static func duration(seconds: Int64) -> String {
        if seconds < 60 {
            return "\(max(seconds, 0)) s"
        }
        let minutes = seconds / 60
        if minutes < 60 {
            return "\(minutes) min"
        }
        return "\(minutes / 60) h \(minutes % 60) min"
    }

    /// A plain digit count for a system badge, such as the Dock tile. No
    /// grouping or rounding: a badge is short by nature.
    static func badgeCount(_ count: Int) -> String {
        String(count)
    }

    /// "1:12" for the time a pairing code has left. Counted, not described,
    /// because "soon" is an adjective standing in for a number.
    static func countdown(seconds: Int64) -> String {
        let clamped = max(seconds, 0)
        return String(format: "%d:%02d", clamped / 60, clamped % 60)
    }

    /// Built once rather than per call: the adapter calls `timeOfDay` for up
    /// to 1,000 rows on every access log tick (docs/engine-contract.md, item
    /// 13), and a fresh `DateFormatter` for each of those was the cost this
    /// caches away. A `DateFormatter` that is only ever read from, never
    /// reconfigured after this, is safe to share that way.
    private static let timeOfDayFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .short
        return formatter
    }()

    /// "14:31" for one access log row. The person's own clock format, so
    /// a twelve hour locale reads "2:31 PM".
    static func timeOfDay(unixSecs: Int64) -> String {
        timeOfDayFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(unixSecs)))
    }

    /// Built once, for the same reason as `timeOfDayFormatter`.
    private static let relativeFormatter: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter
    }()

    /// "2 hours ago" for a time the engine reports in Unix seconds.
    static func relative(unixSecs: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unixSecs))
        return relativeFormatter.localizedString(for: date, relativeTo: Date())
    }

    /// Built once, for the same reason as `timeOfDayFormatter`: `longDate`
    /// runs once per access log entry that falls outside today and
    /// yesterday, while the day grouping decides each row's title.
    private static let longDateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .long
        formatter.timeStyle = .none
        return formatter
    }()

    /// "September 10, 2026" for a time the engine reports in Unix seconds.
    static func longDate(unixSecs: Int64) -> String {
        longDateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(unixSecs)))
    }

    /// "481 920" from the six digits the engine reports, grouped three and
    /// three. A code of another length is returned unchanged.
    static func pairingCode(_ code: String) -> String {
        guard code.count == 6 else { return code }
        let middle = code.index(code.startIndex, offsetBy: 3)
        return "\(code[code.startIndex..<middle]) \(code[middle...])"
    }

    /// Spells out each digit of a pairing code for VoiceOver, so "481 920"
    /// becomes "four eight one, nine two zero". The space between the two
    /// groups becomes the pause a comma gives.
    static func spokenDigits(_ code: String) -> String {
        let words: [Character: String] = [
            "0": "zero", "1": "one", "2": "two", "3": "three", "4": "four",
            "5": "five", "6": "six", "7": "seven", "8": "eight", "9": "nine",
        ]
        return code
            .split(separator: " ")
            .map { group in
                group.compactMap { words[$0] }.joined(separator: " ")
            }
            .joined(separator: ", ")
    }
}
