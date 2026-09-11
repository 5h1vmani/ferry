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
//
// Strings added for docs/ia.md are marked. The same event uses the same
// words on both platforms, so each one has a twin in the phone's
// strings.xml.

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

    /// Whether this Mac advertises and accepts connections. Job 5's only
    /// control, shown in the sidebar footer and the menu bar. New in
    /// docs/ia.md, Presence.
    enum presence {
        static let advertising = "Advertising"
        static let notAdvertising = "Not advertising"
        /// Stated because the failure it causes is silent. Named by its
        /// real name: this Mac, Wi-Fi, USB.
        static let consequence = "This Mac cannot be found on Wi-Fi. USB still works."

        static let accessibilityOn = "Advertising, on"
        static let accessibilityOffFormat = "Not advertising, off. %@"
        static func accessibilityOff(consequence: String) -> String {
            String(format: accessibilityOffFormat, consequence)
        }
    }

    /// The menu bar item. New in docs/ia.md, L0.
    enum menuBar {
        static let openFerry = "Open Ferry"
        static let accessibilityLabel = "Ferry"
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

        /// A transport that is available but not carrying bytes, stated
        /// once beside the badge so a pulled cable is not a surprise. New
        /// in docs/ia.md, Devices.
        static let spareTransportWifi = "Wi-Fi also available"
        static let spareTransportUSB = "USB also available"
        static func spareTransport(_ transport: Transport) -> String {
            switch transport {
            case .usb: return spareTransportUSB
            case .wifi: return spareTransportWifi
            }
        }
    }

    /// The Finder mount, in one line. New in docs/ia.md, Access.
    enum access {
        static let mountReady = "Finder mount ready at"
        static let openInFinder = "Open in Finder"
        static let accessibilityMountReadyFormat = "Finder mount ready at %@"
        static func accessibilityMountReady(path: String) -> String {
            String(format: accessibilityMountReadyFormat, path)
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
        /// "Name. Badge text. Wi-Fi also available. Last seen 2 hours ago."
        /// Empty parts are left out, per the three-part rule: unknown is
        /// left out, not guessed.
        static func accessibilityLabel(
            name: String,
            badge: String,
            spareTransport: String? = nil,
            lastSeen: String? = nil
        ) -> String {
            var parts = [name, badge]
            if let spareTransport {
                parts.append(spareTransport)
            }
            if let lastSeen {
                parts.append(lastSeen)
            }
            return parts.joined(separator: ". ")
        }
    }

    /// The Transfers section. Direction and file counts are new in
    /// docs/ia.md, Transfers.
    enum transfers {
        static let directionPhoneToMac = "Phone to Mac"
        static let directionMacToPhone = "Mac to phone"

        /// Job 7. A word, not a badge: it sits where the direction sits
        /// because it is the same kind of fact, how this came to exist.
        static let originAutomatic = "Automatic"

        static let filesProgressFormat = "%1$d of %2$d files"
        static func filesProgress(done: Int, total: Int) -> String {
            String(format: filesProgressFormat, done, total)
        }

        static let fileCountFormat = "%d files"
        static func fileCount(_ count: Int) -> String {
            String(format: fileCountFormat, count)
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

    /// The bottom of the depth axis. New in docs/ia.md, The chunk
    /// disclosure.
    enum chunks {
        static let show = "Show chunks"

        static let verifiedFormat = "%1$d of %2$d verified"
        static func verified(verified: Int, total: Int) -> String {
            String(format: verifiedFormat, verified, total)
        }

        static let verifiedSummaryFormat = "%1$d of %2$d chunks verified."
        static func verifiedSummary(verified: Int, total: Int) -> String {
            String(format: verifiedSummaryFormat, verified, total)
        }

        static let failedChunkFormat = "Chunk %d failed to verify."
        static func failedChunk(index: Int) -> String {
            String(format: failedChunkFormat, index)
        }

        static let accessibilityHint = "Shows the chunks of this transfer"
    }

    /// The access log. L5, job 9. New in docs/ia.md, The access log.
    enum accessLog {
        static let section = "Access log"
        static let empty = "No access yet."
        static let retention = "Kept for 30 days."
        static let revealInFinder = "Reveal in Finder"
        static let today = "Today"
        static let yesterday = "Yesterday"

        /// The sentence's subject, always a name and never "you". A log is
        /// read months later, out of context.
        static let thisMac = "This Mac"
        static func subject(actor: AccessActor, peerName: String) -> String {
            switch actor {
            case .peer: return peerName
            case .thisDevice: return thisMac
            }
        }

        /// The file operations layer's own words, so a log line and a
        /// protocol trace agree.
        static func verb(_ verb: AccessVerb) -> String {
            switch verb {
            case .list: return "listed"
            case .stat: return "checked"
            case .read: return "read"
            case .write: return "wrote"
            case .truncate: return "truncated"
            case .rename: return "renamed"
            case .mkdir: return "created"
            case .delete: return "deleted"
            }
        }

        static let entriesFormat = "%d entries"
        static func entries(_ count: Int) -> String {
            String(format: entriesFormat, count)
        }

        /// The sentence first, because it is what a person is looking for,
        /// then the numbers that qualify it.
        static func accessibilityLabel(sentence: AttributedString, time: String, amount: String?) -> String {
            var parts = [String(sentence.characters), time]
            if let amount {
                parts.append(amount)
            }
            return parts.joined(separator: ". ")
        }
    }

    /// Job 7's switch and its lines. New in docs/ia.md, Automatic.
    enum automatic {
        static let section = "Automatic"

        static let copyNewPhotosFormat = "Copy new photos from %@"
        static func copyNewPhotos(from deviceName: String) -> String {
            String(format: copyNewPhotosFormat, deviceName)
        }

        /// The sentence that separates job 7 from the two-way sync Ferry
        /// refuses. Stated, never implied.
        static let ruleFormat = "From %1$@. To %2$@. Never deleted, never written back."
        static func rule(source: String, destination: String) -> String {
            String(format: ruleFormat, source, destination)
        }

        /// A count and a time. Not "up to date".
        static let lastRunFormat = "Last copied %1$d files, %2$@."
        static func lastRun(files: Int, relative: String) -> String {
            String(format: lastRunFormat, files, relative)
        }

        /// Said plainly rather than showing a switch that does nothing.
        static let notBuiltYet = "Ferry cannot copy on its own yet."
    }

    enum errorBlock {
        /// Read together as one element: what stopped, why, what to do, in
        /// that order (docs/components.md, ErrorBlock). The Retry
        /// button, when present, is a separate control and speaks for
        /// itself.
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

        /// One candidate in the Found state (docs/ia.md, Pairing, the
        /// Mac). A phone on the cable is the only one on that cable, so it
        /// needs no short code. A phone on Wi-Fi shows the four characters
        /// it shows on its own screen.
        static let candidateUSB = "Phone over USB"
        static let candidateWifiFormat = "Phone on Wi-Fi%@%@"
        static func candidateWifi(shortCode: String) -> String {
            String(format: candidateWifiFormat, common.dotSeparator, shortCode)
        }

        // The scan method. New in docs/ia.md, Pairing, the Mac, by scan.
        static let scanThis = "Scan this with the phone."
        static let scanTheCode = "Scan the Mac's code"
        static let useCodeInstead = "Use a pairing code instead"
        static let showANewCode = "Show a new code"
        static let accessibilityCode = "Pairing code, as a square code for the phone's camera"

        static let expiresInFormat = "The code expires in %@."
        static func expiresIn(_ countdown: String) -> String {
            String(format: expiresInFormat, countdown)
        }

        /// TODO(engine 12): shown while the payload is a placeholder, so
        /// nobody tries to scan a code that cannot work.
        static let placeholderNote = "This code cannot be scanned yet. Use a pairing code."
        static let placeholderPayload = "ferry:placeholder"

        static let wantsToPairFormat = "%@ wants to pair."
        static func wantsToPair(name: String) -> String {
            String(format: wantsToPairFormat, name)
        }

        static let scannedOverFormat = "It scanned this Mac's code over %@. Pairing lets it read and write the shared folders."
        static func scannedOver(transport: Transport) -> String {
            let name = transport == .usb ? transportBadge.usb : transportBadge.wifi
            return String(format: scannedOverFormat, name)
        }

        static let pair = "Pair"
        static let refuse = "Refuse"

        /// The payload could not be encoded. Not a pairing failure, and it
        /// has no cause worth guessing at, so the "why" is left out.
        static let codeNotShownStopped = "The code could not be shown."
        static let codeNotShownWhy = ""
        static let codeNotShownToDo = "Use a pairing code."

        /// The two minute timeout, as a count. The engine does not publish
        /// the deadline yet (docs/engine-contract.md, item 10), so nothing calls
        /// this on the Mac; the phone's screen is designed against it and
        /// its twin is already in strings.xml.
        static let stopsInFormat = "Pairing stops in %@."
        static func stopsIn(_ countdown: String) -> String {
            String(format: stopsInFormat, countdown)
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

    /// Browsing the phone's shared folder. This is the only view in Ferry
    /// with a loading state, and it is honest: a folder listing is a round
    /// trip to another device.
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

        static let copyToMacAccessibilityFormat = "Copy %@ to Mac"
        static func copyToMacAccessibility(name: String) -> String {
            String(format: copyToMacAccessibilityFormat, name)
        }
    }

    enum settings {
        static let choose = "Choose"

        // Shared folders. New in docs/ia.md, Settings, the Mac.
        static let sharedFolders = "Shared folders"
        static let sharedFoldersFooter = "Paired phones can read and write these folders. Nothing outside them is served."
        static let addAFolder = "Add a folder"
        static let stopSharing = "Stop sharing"
        static let whereFilesLand = "Where pulled files land"

        static let rootAccessibilityFormat = "%1$@, shared from %2$@"
        static func rootAccessibility(name: String, path: String) -> String {
            String(format: rootAccessibilityFormat, name, path)
        }
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

    /// The Finder mount, item 6. A NetFS failure is not an engine error, so
    /// its words are here and not in the generated table, the same choice
    /// `keyStore` makes.
    enum mount {
        static let failedStopped = "The Finder mount did not start."
        static let failedWhyFormat = "%@"
        static func failedWhy(reason: String) -> String {
            String(format: failedWhyFormat, reason)
        }
        static let failedToDo = "Try again. If it keeps failing, restart Ferry."
    }
}
