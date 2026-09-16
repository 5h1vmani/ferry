// Settings: the shared folders list, where pulled files land, and the
// persisted values and engine-construction helpers behind both.
//
// Split out of EngineModel.swift. `docs/audits/principles.md`, M6.

import Foundation

extension EngineModel {
    /// One stored root: its own keys under `rootsKey`, encoded as JSON so
    /// UserDefaults holds one value rather than three parallel arrays.
    private struct StoredRoot: Codable {
        let name: String
        let path: String
        let writable: Bool
    }

    /// The versioned shape written under `rootsKey`. `version` lets a
    /// future field change tell a stored value apart from a corrupt one
    /// instead of guessing. `docs/audits/fable-engineering.md`, finding 3.
    private struct StoredRoots: Codable {
        let version: Int
        let roots: [StoredRoot]
    }

    private static let rootsKey = "sharedRoots"
    private static let rootsVersion = 1
    private static let downloadPathKey = "downloadPath"
    private static let displayNameLimit = 64

    // MARK: - Shared folders

    /// Serves exactly this set of folders, under the names already known
    /// for a path that was already served, and a name guessed from the
    /// folder itself for a path that is new. No engine restart: `set_roots`
    /// reaches an already-connected peer on its next operation.
    ///
    /// While the engine has not started, such as before storage permission
    /// is granted, this only persists the choice: `start` reads it back.
    func setRoots(_ paths: [String]) {
        guard !paths.isEmpty else { return }
        let known = Dictionary(uniqueKeysWithValues: roots.map { ($0.path, $0) })
        let wanted = paths.map { path in
            known[path] ?? SharedRootSnapshot(
                name: (path as NSString).lastPathComponent,
                path: path,
                isWritable: true
            )
        }
        if let engine {
            do {
                try engine.setRoots(roots: wanted.map(EngineModel.engineRoot))
            } catch {
                report(error)
                return
            }
            roots = EngineAdapter.roots(engine.roots())
        } else {
            roots = wanted
        }
        EngineModel.storeRoots(roots)
    }

    /// Where pulled files land. No engine restart: `set_download_dir` takes
    /// effect for the next pull. While the engine has not started, this
    /// only persists the choice, the same as `setRoots`.
    func setDownloadPath(_ path: String) {
        if let engine {
            do {
                try engine.setDownloadDir(path: path)
            } catch {
                report(error)
                return
            }
        }
        downloadPath = path
        UserDefaults.standard.set(path, forKey: EngineModel.downloadPathKey)
    }

    // MARK: - Where things live

    /// Desktop and Downloads, both writable: names a person recognises,
    /// where the first version's single "Ferry" folder was a name Ferry
    /// made up.
    private static func defaultRoots() -> [SharedRootSnapshot] {
        [NSHomeDirectory() + "/Desktop", NSHomeDirectory() + "/Downloads"].map { path in
            SharedRootSnapshot(
                name: (path as NSString).lastPathComponent,
                path: path,
                isWritable: true
            )
        }
    }

    /// No stored value at all is a first run, so it returns the writable
    /// defaults. A stored value that does not decode, whether corrupt or
    /// from a shape this build no longer reads, never falls back to those
    /// writable folders: that would silently share Desktop and Downloads
    /// on a change nobody asked for. It returns an empty list instead and
    /// says so through `decodeFailed`. `docs/audits/fable-engineering.md`,
    /// finding 3.
    static func storedRoots() -> (roots: [SharedRootSnapshot], decodeFailed: Bool) {
        guard let data = UserDefaults.standard.data(forKey: rootsKey) else {
            return (defaultRoots(), false)
        }
        guard
            let stored = try? JSONDecoder().decode(StoredRoots.self, from: data),
            stored.version == rootsVersion
        else {
            return ([], true)
        }
        guard !stored.roots.isEmpty else {
            return (defaultRoots(), false)
        }
        return (
            stored.roots.map {
                SharedRootSnapshot(name: $0.name, path: $0.path, isWritable: $0.writable)
            },
            false
        )
    }

    private static func storeRoots(_ roots: [SharedRootSnapshot]) {
        let stored = StoredRoots(
            version: rootsVersion,
            roots: roots.map { StoredRoot(name: $0.name, path: $0.path, writable: $0.isWritable) }
        )
        guard let data = try? JSONEncoder().encode(stored) else { return }
        UserDefaults.standard.set(data, forKey: rootsKey)
    }

    /// A `SharedRootSnapshot`, as the engine's `Config` and `set_roots`
    /// take it.
    static func engineRoot(_ root: SharedRootSnapshot) -> Root {
        Root(name: root.name, path: root.path, writable: root.isWritable)
    }

    static func storedDownloadPath() -> String {
        if let stored = UserDefaults.standard.string(forKey: downloadPathKey), !stored.isEmpty {
            return stored
        }
        return NSHomeDirectory() + "/Downloads/Ferry"
    }

    /// The engine's own files: paired devices and transfer records. Marked
    /// `nonisolated`, with `makeDirectory(at:)`, so `start()` can run this
    /// inside a detached task rather than on the main actor: it touches no
    /// instance state, only the disk. `docs/audits/principles.md`, M21.
    nonisolated static func makeDataDirectory() throws -> String {
        let path = NSHomeDirectory() + "/Library/Application Support/Ferry"
        try makeDirectory(at: path)
        return path
    }

    nonisolated private static func makeDirectory(at path: String) throws {
        try FileManager.default.createDirectory(
            atPath: path,
            withIntermediateDirectories: true
        )
    }

    /// The name sent to the other device. At most 64 bytes, which is what
    /// the engine accepts.
    static func displayName() -> String {
        let name = Host.current().localizedName ?? S.app.defaultDeviceName
        var trimmed = name
        while trimmed.utf8.count > displayNameLimit {
            trimmed.removeLast()
        }
        return trimmed.isEmpty ? S.app.defaultDeviceName : trimmed
    }

    /// A GUI app is launched with a short PATH that has no adb on it. The
    /// engine reads PATH when it is created to find adb. Without this, USB
    /// is never available. See docs/decisions/0010-no-mac-sandbox.md.
    ///
    /// adb is not in Homebrew's bin folder. The Homebrew command line tools
    /// put it under share, and Android Studio puts it under the home
    /// folder. Every known place is added, and the one that exists wins.
    static func addAdbToPath() {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let known = [
            "/opt/homebrew/share/android-commandlinetools/platform-tools",
            "/usr/local/share/android-commandlinetools/platform-tools",
            home + "/Library/Android/sdk/platform-tools",
            "/opt/homebrew/bin",
            "/usr/local/bin",
        ]
        let current = ProcessInfo.processInfo.environment["PATH"] ?? ""
        let present = Set(current.split(separator: ":").map(String.init))
        let missing = known.filter { !present.contains($0) }
        if missing.isEmpty {
            return
        }
        let prefix = missing.joined(separator: ":")
        let combined = current.isEmpty ? prefix : prefix + ":" + current
        setenv("PATH", combined, 1)
    }
}
