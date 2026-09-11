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
    /// Tries a mount directory named after the device first, under
    /// `~/Library/Application Support/Ferry/mounts/<device name>`, so the
    /// volume Finder shows is named for the phone rather than a NetFS
    /// default. Falls back to the OS's own default location, ordinarily
    /// under `/Volumes`, when that directory cannot be made or NetFS
    /// refuses to mount at it.
    ///
    /// # Errors
    ///
    /// Throws `FinderMountError.failed` when both tries fail.
    static func mount(endpoint: MountEndpoint, deviceName: String) throws -> String {
        guard let url = URL(string: endpoint.url) else {
            throw FinderMountError.failed(reason: "The mount address did not parse.")
        }
        if let namedDirectory = makeNamedMountDirectory(for: deviceName),
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

    private static func makeNamedMountDirectory(for deviceName: String) -> URL? {
        let base = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Ferry/mounts", isDirectory: true)
        let directory = base.appendingPathComponent(deviceName, isDirectory: true)
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
    private static func mountOnce(
        url: URL,
        mountDirectory: URL?,
        user: String,
        password: String
    ) throws -> String {
        var mountPoints: Unmanaged<CFArray>?
        let openOptions = NSMutableDictionary()
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
