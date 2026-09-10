// Plain number formatting shared by components and sample data. Ferry states
// numbers instead of adjectives (docs/voice.md, rule 5), so these helpers
// turn raw bytes, seconds, and dates into the exact shapes the components
// print, such as "38 MB/s" or "2.1 GB".

import Foundation

enum FerryFormat {
    /// "38 MB/s", for the visible label. Decimal megabytes, rounded.
    static func speed(bytesPerSec: Int) -> String {
        let mb = Double(bytesPerSec) / 1_000_000
        return "\(Int(mb.rounded())) MB/s"
    }

    /// "38 megabytes per second", for VoiceOver. Same value, spoken out.
    static func speedSpoken(bytesPerSec: Int) -> String {
        let mb = Double(bytesPerSec) / 1_000_000
        return "\(Int(mb.rounded())) megabytes per second"
    }

    /// "2.1 GB" for a byte count of a gigabyte or more, else "120 MB".
    static func bytes(_ count: Int64) -> String {
        let value = Double(count)
        if value >= 1_000_000_000 {
            return String(format: "%.1f GB", value / 1_000_000_000)
        }
        return "\(Int((value / 1_000_000).rounded())) MB"
    }

    /// "3 min" for a duration in seconds. Ferry transfers run in minutes,
    /// never hours, so no larger unit is needed in phase 1.
    static func minutes(_ seconds: Int) -> String {
        let minutes = max(1, Int((Double(seconds) / 60).rounded()))
        return "\(minutes) min"
    }

    /// "2 hours ago" for a past date, using the system's own phrasing.
    static func relative(_ date: Date) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(for: date, relativeTo: Date())
    }

    /// "September 10, 2026" for a paired date.
    static func longDate(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .long
        formatter.timeStyle = .none
        return formatter.string(from: date)
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
