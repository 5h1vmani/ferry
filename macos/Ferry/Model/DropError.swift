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
        }
    }
}
