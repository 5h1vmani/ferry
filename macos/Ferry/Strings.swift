// Every user-facing string in Ferry for the Mac, grouped by the screen or
// component that owns it. This is the only file with English prose in it.
// A view never types a word; it reads a value from S. Words follow
// docs/voice.md: state what is true, name things by their real names,
// numbers instead of adjectives, a button is a verb.
//
// Some strings carry a number or a name that is only known at render time
// (a speed, a file size, a device name). Those stay a `static let` format
// template here, with a small `static func` next to it that fills the
// template in. The English itself never leaves this file.
//
// The words for an engine error are not here. They are generated into
// Generated/Errors.swift from design/errors.json.

import Foundation

enum S {
    enum app {
        static let name = "Ferry"
        /// Sent to the phone when this Mac has no name of its own.
        static let defaultDeviceName = "Mac"
    }

    /// Words shared by more than one screen or component.
    enum common {
        static let cancel = "Cancel"
        static let confirm = "Confirm"
        static let retry = "Retry"
        static let connecting = "Connecting"
        static let notReachable = "Not reachable"
        static let dotSeparator = " · "

        /// For an error code that is not in the generated table. That
        /// should not happen, so the code itself is shown. A guess would
        /// be worse than the code.
        static let unknownErrorStopped = "The action stopped."
        static let unknownErrorWhyFormat = "Ferry has no words for the code %@."
        static func unknownErrorWhy(code: String) -> String {
            String(format: unknownErrorWhyFormat, code)
        }
        static let unknownErrorToDo = "Report this code."
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
        static let bytesRemainingFormat = "%@ remaining"
        static func bytesRemaining(_ bytes: String) -> String {
            String(format: bytesRemainingFormat, bytes)
        }

        /// The engine queues a transfer past the fourth one, and while the
        /// device is not reachable.
        static let queued = "Queued."

        static let paused = "Paused."

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

        /// One candidate in the Found state (docs/ia.md, Pairing, Found).
        /// A phone on the cable is the only one on that cable, so it needs
        /// no short code. A phone on Wi-Fi shows the four characters it
        /// shows on its own screen.
        static let candidateUSB = "Phone over USB"
        static let candidateWifiFormat = "Phone on Wi-Fi%@%@"
        static func candidateWifi(shortCode: String) -> String {
            String(format: candidateWifiFormat, common.dotSeparator, shortCode)
        }
    }

    enum deviceDetail {
        static let filesSection = "Files"
        static let transfersSection = "Transfers"
        static let infoSection = "Info"
        static let noTransfers = "No transfers."
        static let pairedLabel = "Paired"
        static let keyFingerprintLabel = "Key fingerprint"
        static let forgetThisPhone = "Forget this phone"
    }

    /// Browsing the phone's shared folder. docs/ia.md leaves this screen to
    /// phase 2, so these words are new and follow docs/voice.md.
    enum files {
        static let root = "/"
        static let goUp = "Go up"
        static let copyToMac = "Copy to Mac"
        static let emptyFolder = "This folder is empty."
        static let reading = "Reading the folder."

        static let folderAccessibilityFormat = "%@, folder"
        static func folderAccessibility(name: String) -> String {
            String(format: folderAccessibilityFormat, name)
        }

        static let fileAccessibilityFormat = "%@, %@"
        static func fileAccessibility(name: String, size: String) -> String {
            String(format: fileAccessibilityFormat, name, size)
        }
    }

    enum settings {
        static let sharedFolderLabel = "Shared folder"
        static let choose = "Choose"
    }

    /// The Keychain holds this Mac's key. A Keychain failure is not an
    /// engine error, so its words are here and not in the generated table.
    enum keyStore {
        static let keychainStopped = "Ferry could not start."
        static let keychainWhy = "The Keychain refused the device key."
        static let keychainToDo = "Restart Ferry. If it repeats, unlock the login keychain."

        static let wrongSizeStopped = "The stored key is unusable."
        static let wrongSizeWhy = "It is not the size Ferry expects."
        static let wrongSizeToDo = "Delete the Ferry device key in Keychain Access. Ferry makes a new one."
    }
}
