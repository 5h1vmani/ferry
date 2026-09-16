// What this app refuses before a drop, a Send files action, or the Finder
// service ever reaches the engine (docs/ux-fix-plan.md, item 3). Not a
// FerryError: the refusal happens on this side, the same choice
// `KeyStoreError` and `FinderMountError` make for their own failures.

import Foundation

enum DropError: Error {
    /// A folder was among the items to push. The engine has no way to
    /// push a folder yet (docs/ux-fix-plan.md, "Not in this pass").
    case folderNotSupported
    /// The device lists no root to land files in.
    case noLandingFolder
    /// One of the URLs to send was not a file on this Mac.
    /// `docs/audits/ux-gestures.md`, finding 4.
    case nonFileURL
    /// A drop landed on a device that is not reachable right now.
    /// `docs/audits/ux-gestures.md`, finding 11.
    case deviceNotReachable
    /// A drop on the Dock icon found no device to send to.
    /// `docs/audits/ux-gestures.md`, finding 11.
    case noDevice
}

extension DropError {
    func threePart(canRetry: Bool) -> ThreePartError {
        switch self {
        case .folderNotSupported:
            return ThreePartError(
                whatStopped: S.drop.folderStopped,
                why: S.drop.folderWhy,
                whatToDo: S.drop.folderToDo,
                canRetry: canRetry
            )
        case .noLandingFolder:
            return ThreePartError(
                whatStopped: S.drop.noLandingFolderStopped,
                why: S.drop.noLandingFolderWhy,
                whatToDo: S.drop.noLandingFolderToDo,
                canRetry: canRetry
            )
        case .nonFileURL:
            return ThreePartError(
                whatStopped: S.drop.nonFileURLStopped,
                why: S.drop.nonFileURLWhy,
                whatToDo: S.drop.nonFileURLToDo,
                canRetry: canRetry
            )
        case .deviceNotReachable:
            return ThreePartError(
                whatStopped: S.drop.deviceNotReachableStopped,
                why: S.drop.deviceNotReachableWhy,
                whatToDo: S.drop.deviceNotReachableToDo,
                canRetry: canRetry
            )
        case .noDevice:
            return ThreePartError(
                whatStopped: S.drop.noDeviceStopped,
                why: S.drop.noDeviceWhy,
                whatToDo: S.drop.noDeviceToDo,
                canRetry: canRetry
            )
        }
    }
}
