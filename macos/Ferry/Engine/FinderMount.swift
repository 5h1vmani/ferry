// Mounts a device's WebDAV bridge in Finder, and unmounts it.
//
// docs/engine-contract.md, item 6. The engine only serves the bridge; this
// file is what makes the OS put it in Finder. It calls the NetFS API with
// the credentials passed in memory, never on a command line, and never
// shows any UI of its own. `EngineModel` calls this off the main actor.

import AppKit
import Foundation
import NetFS

/// What can go wrong mounting or unmounting. Not a FerryError: the failure
/// happens on this side, in the OS call, not in the engine.
enum FinderMountError: Error {
    /// `reason` is a short, concrete fact: a NetFS status code, or which
    /// step failed. Never a sentence Ferry composed about it.
    case failed(reason: String)
}

extension FinderMountError {
    /// The words a person reads. A NetFS failure is not a FerryError, so it
    /// does not go through Generated/Errors.swift, the same choice
    /// `KeyStoreError` makes for the Keychain.
    func threePart(canRetry: Bool) -> ThreePartError {
        switch self {
        case let .failed(reason):
            return ThreePartError(
                whatStopped: S.mount.failedStopped,
                why: S.mount.failedWhy(reason: reason),
                whatToDo: S.mount.failedToDo,
                canRetry: canRetry
            )
        }
    }
}

enum FinderMount {
    /// Mounts `endpoint` through the NetFS API and returns the POSIX path
    /// the OS put it at.
    ///
    /// Tries a mount directory named after the device's own key hex first,
    /// under `~/Library/Application Support/Ferry/mounts/<device key
    /// hex>`. The device's display name is untrusted text a peer chose for
    /// itself: it can hold `/` or `..`, so it never becomes a path
    /// component (S7). NetFS has no option key of its own for a volume's
    /// display name separate from where it is mounted (checked against
    /// this SDK's `NetFS.h`, `mountOnce`'s own comment), so the volume
    /// Finder shows is whatever the OS derives from the mount directory;
    /// the device's real name still reaches Finder as the mount root's
    /// `displayname` property, `docs/engine-contract.md`, item 6, N4.
    ///
    /// Falls back to the OS's own default location, ordinarily under
    /// `/Volumes`, when the named directory cannot be made or NetFS
    /// refuses to mount at it.
    ///
    /// # Errors
    ///
    /// Throws `FinderMountError.failed` when both tries fail.
    static func mount(endpoint: MountEndpoint, deviceKeyHex: String) throws -> String {
        guard let url = URL(string: endpoint.url) else {
            throw FinderMountError.failed(reason: "The mount address did not parse.")
        }
        if let namedDirectory = makeNamedMountDirectory(for: deviceKeyHex),
            let path = try? mountOnce(
                url: url,
                mountDirectory: namedDirectory,
                user: endpoint.user,
                password: endpoint.password
            ) {
            return path
        }
        return try mountOnce(url: url, mountDirectory: nil, user: endpoint.user, password: endpoint.password)
    }

    /// Unmounts the volume at `path`. Errors are swallowed: this runs at
    /// quit and at forget, where nothing can act on a failure, and a
    /// volume that is already gone is the outcome asked for either way.
    static func unmount(path: String) {
        try? NSWorkspace.shared.unmountAndEjectDevice(at: URL(fileURLWithPath: path))
    }

    /// `deviceKeyHex` is lowercase hex: always a safe single path
    /// component, unlike the device's own display name (S7).
    private static func makeNamedMountDirectory(for deviceKeyHex: String) -> URL? {
        let base = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Ferry/mounts", isDirectory: true)
        let directory = base.appendingPathComponent(deviceKeyHex, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return directory
        } catch {
            return nil
        }
    }

    /// One call to `NetFSMountURLSync`. `mountDirectory` nil asks NetFS for
    /// its own default location; non-nil asks it to mount directly on that
    /// folder (`kNetFSMountAtMountDirKey`) rather than creating a folder
    /// beneath it.
    ///
    /// The open options ask NetFS for no UI of its own (`kNAUIOptionKey` =
    /// `kNAUIOptionNoUI`) and to allow this loopback mount at all
    /// (`kNetFSAllowLoopbackKey`): both are declared in this SDK's
    /// `NetFS.h` (S8), so both are set here rather than one being left
    /// out.
    private static func mountOnce(
        url: URL,
        mountDirectory: URL?,
        user: String,
        password: String
    ) throws -> String {
        var mountPoints: Unmanaged<CFArray>?
        let openOptions = NSMutableDictionary()
        openOptions[kNAUIOptionKey as String] = kNAUIOptionNoUI as String
        openOptions[kNetFSAllowLoopbackKey as String] = true
        let mountOptions = NSMutableDictionary()
        if mountDirectory != nil {
            mountOptions[kNetFSMountAtMountDirKey as String] = true
        }
        let result = NetFSMountURLSync(
            url as CFURL,
            mountDirectory as CFURL?,
            user as CFString,
            password as CFString,
            openOptions,
            mountOptions,
            &mountPoints
        )
        guard result == 0 else {
            throw FinderMountError.failed(reason: "NetFS returned \(result).")
        }
        guard
            let points = mountPoints?.takeRetainedValue() as? [String],
            let first = points.first
        else {
            throw FinderMountError.failed(reason: "NetFS reported success with no mount point.")
        }
        return first
    }
}
