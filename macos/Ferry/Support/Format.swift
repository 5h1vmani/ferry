// Plain number formatting shared by the components. Ferry states numbers
// instead of adjectives (docs/voice.md, rule 5), so these helpers turn the
// raw bytes and Unix seconds the engine reports into the exact shapes the
// components print, such as "38 MB/s" or "2.1 GB".

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

    /// "2 hours ago" for a time the engine reports in Unix seconds.
    static func relative(unixSecs: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unixSecs))
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(for: date, relativeTo: Date())
    }

    /// "September 10, 2026" for a time the engine reports in Unix seconds.
    static func longDate(unixSecs: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unixSecs))
        let formatter = DateFormatter()
        formatter.dateStyle = .long
        formatter.timeStyle = .none
        return formatter.string(from: date)
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
