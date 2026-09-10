// Small additions to the types the engine exports through UniFFI. The
// generated file in Generated/ferry_runtime.swift is never edited, so
// anything SwiftUI needs from those types is added here.
//
// SwiftUI lists need Identifiable. The engine already gives each value a
// stable identity, so each conformance below points at the field that
// holds it.

import Foundation

extension DeviceInfo: Identifiable {
    /// The public key is the identity of a device. See docs/ia.md.
    public var id: String { keyHex }
}

extension TransferInfo: Identifiable {}

extension PairingCandidate: Identifiable {}

extension Entry: Identifiable {
    /// A name is unique inside one folder, and one folder is all a list
    /// shows at a time.
    public var id: String { name }
}
