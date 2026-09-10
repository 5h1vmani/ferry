// Every user-facing string in Ferry for the Mac, grouped by the screen or
// component that owns it. This is the only file with English prose in it.
// A view never types a word; it reads a value from S. Words follow
// docs/voice.md: state what is true, name things by their real names,
// numbers instead of adjectives, a button is a verb.
//
// Some strings carry a number or a name that is only known at render time
// (a speed, a file count, a device name). Those stay a `static let` format
// template here, with a small `static func` next to it that fills the
// template in. The English itself never leaves this file.

import Foundation

enum S {
    enum app {
        static let name = "Ferry"
    }

    /// Words shared by more than one screen or component.
    enum common {
        static let cancel = "Cancel"
        static let confirm = "Confirm"
        static let retry = "Retry"
        static let connecting = "Connecting"
        static let notReachable = "Not reachable"
        static let dotSeparator = " · "
    }

    enum devices {
        static let sidebarTitle = "Devices"
        static let noPhonePaired = "No phone paired."
        static let noPhoneSelected = "No phone selected."
        static let pairAPhone = "Pair a phone"
        static let lastSeenFormat = "Last seen %@"
        static func lastSeen(_ relative: String) -> String {
            String(format: lastSeenFormat, relative)
        }
    }

    enum transportBadge {
        static let usb = "USB"
        static let wifi = "Wi-Fi"

        static let usbMovingFormat = "USB%@%@"
        static let wifiMovingFormat = "Wi-Fi%@%@"
        static func usbMoving(speed: String) -> String {
            String(format: usbMovingFormat, common.dotSeparator, speed)
        }
        static func wifiMoving(speed: String) -> String {
            String(format: wifiMovingFormat, common.dotSeparator, speed)
        }

        static let accessibilityUSB = "Connected over USB"
        static let accessibilityWifi = "Connected over Wi-Fi"
        static let accessibilityUSBSpeedFormat = "Connected over USB, %@"
        static let accessibilityWifiSpeedFormat = "Connected over Wi-Fi, %@"
        static func accessibilityUSBSpeed(_ spoken: String) -> String {
            String(format: accessibilityUSBSpeedFormat, spoken)
        }
        static func accessibilityWifiSpeed(_ spoken: String) -> String {
            String(format: accessibilityWifiSpeedFormat, spoken)
        }
    }

    enum deviceRow {
        /// "Name. Badge text. Last seen 2 hours ago." Empty parts are left
        /// out, per the three-part rule: unknown is left out, not guessed.
        static func accessibilityLabel(name: String, badge: String, lastSeen: String?) -> String {
            var parts = [name, badge]
            if let lastSeen {
                parts.append(lastSeen)
            }
            return parts.joined(separator: ". ")
        }
    }

    enum progressLine {
        static let filesProgressFormat = "%d of %d files"
        static func filesProgress(done: Int, total: Int) -> String {
            String(format: filesProgressFormat, done, total)
        }

        static let bytesRemainingFormat = "%@ remaining"
        static func bytesRemaining(_ bytes: String) -> String {
            String(format: bytesRemainingFormat, bytes)
        }

        static let paused = "Paused"
        /// The exact words for a transfer paused by a dropped USB cable
        /// (docs/ia.md, Transfers, Paused). Ferry uses the same words for
        /// every cable-disconnect pause, on either platform.
        static let cableDisconnectedReason = "The cable was disconnected. Reconnect to continue."

        static func doneSummary(files: Int, size: String, duration: String) -> String {
            "\(files) files\(common.dotSeparator)\(size)\(common.dotSeparator)\(duration)"
        }

        static let accessibilityTransferringFormat = "Transferring, %d percent"
        static func accessibilityTransferring(percent: Int) -> String {
            String(format: accessibilityTransferringFormat, percent)
        }
    }

    enum errorBlock {
        /// Read together as one element: what stopped, why, what to do, in
        /// that order (docs/components.md, ErrorBlock). The Retry button,
        /// when present, is a separate control and speaks for itself.
        static func accessibilityLabel(whatStopped: String, why: String, whatToDo: String) -> String {
            [whatStopped, why, whatToDo].joined(separator: " ")
        }
    }

    enum pairing {
        static let title = "Pair a phone"
        static let waitingHeadline = "Looking for a phone."
        static let waitingBody = "Plug in a cable, or open Ferry on the phone and turn on pairing."
        static let codeInstruction = "Confirm this matches on the phone."
        static let accessibilityConfirmed = "Paired"

        static let codeAccessibilityFormat = "Pairing code, %@"
        static func codeAccessibility(_ spokenDigits: String) -> String {
            String(format: codeAccessibilityFormat, spokenDigits)
        }
    }

    enum deviceDetail {
        static let transfersSection = "Transfers"
        static let infoSection = "Info"
        static let noTransfers = "No transfers."
        static let pairedLabel = "Paired"
        static let keyFingerprintLabel = "Key fingerprint"
        static let forgetThisPhone = "Forget this phone"
    }

    enum settings {
        static let windowTitle = "Settings"
        static let sharedFolderLabel = "Shared folder"
        static let choose = "Choose"
    }

    /// Scaffold-only chrome for viewing every state without a running
    /// engine. None of this is shown to a person using the shipped app; it
    /// exists so every screen state in docs/ia.md can be checked by eye.
    enum debug {
        static let sampleStateMenu = "Sample data"
        static let sampleStateEmpty = "Empty"
        static let sampleStatePopulated = "Populated"
        static let pairingStatePicker = "Pairing state"
        static let pairingStateWaiting = "Waiting"
        static let pairingStateFound = "Found"
        static let pairingStateCode = "Code"
        static let pairingStateConfirmed = "Confirmed"
        static let pairingStateFailed = "Failed"
    }
}
