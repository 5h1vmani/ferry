// The Mac's long-lived key, kept in the Keychain.
//
// The engine does not store keys. It is handed one at construction and
// wipes it when it is dropped, so the platform owns the storage. On the
// Mac that is one generic password item: service `app.ferry.mac`, account
// `device-key`. Its data is the 32 private bytes followed by the 32 public
// bytes.
//
// Key bytes are never printed and never logged.

import Foundation
import Security

/// What can go wrong reading or writing the stored key.
enum KeyStoreError: Error {
    /// The Keychain refused a read, a write, or a delete.
    case keychain(OSStatus)
    /// The stored item is not 64 bytes, so it is not a key pair.
    case wrongSize
}

extension KeyStoreError {
    /// The words a person reads. A Keychain failure is not a FerryError,
    /// so it does not go through Generated/Errors.swift.
    func threePart(canRetry: Bool) -> ThreePartError {
        switch self {
        case .keychain:
            return ThreePartError(
                whatStopped: S.keyStore.keychainStopped,
                why: S.keyStore.keychainWhy,
                whatToDo: S.keyStore.keychainToDo,
                canRetry: canRetry
            )
        case .wrongSize:
            return ThreePartError(
                whatStopped: S.keyStore.wrongSizeStopped,
                why: S.keyStore.wrongSizeWhy,
                whatToDo: S.keyStore.wrongSizeToDo,
                canRetry: canRetry
            )
        }
    }
}

enum KeyStore {
    private static let service = "app.ferry.mac"
    private static let account = "device-key"
    private static let halfLength = 32

    /// Reads this Mac's key pair, and makes one on first launch.
    static func loadOrCreate() throws -> KeyPair {
        if let stored = try load() {
            return stored
        }
        let fresh = try generateKey()
        try save(fresh)
        return fresh
    }

    private static func load() throws -> KeyPair? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess else {
            throw KeyStoreError.keychain(status)
        }
        guard let data = item as? Data, data.count == halfLength * 2 else {
            throw KeyStoreError.wrongSize
        }
        return KeyPair(
            private: Data(data.prefix(halfLength)),
            public: Data(data.suffix(halfLength))
        )
    }

    private static func save(_ pair: KeyPair) throws {
        var bytes = Data()
        bytes.append(pair.`private`)
        bytes.append(pair.`public`)
        let attributes: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecValueData as String: bytes,
            // The key is only needed while this Mac is unlocked and in use,
            // and it never leaves this Mac.
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw KeyStoreError.keychain(status)
        }
    }
}
