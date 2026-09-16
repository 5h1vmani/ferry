// An error as a person reads it: what stopped, why, what to do. This is
// the three-part rule from docs/voice.md, held as data so ErrorBlock can
// draw it and a screen reader can speak it in order.
//
// Every error the engine raises is a FerryError with a code. The words for
// a code live in Generated/Errors.swift, which is generated from
// design/errors.json. No English is written here.

import Foundation

struct ThreePartError: Equatable {
    let whatStopped: String
    let why: String
    let whatToDo: String
    let canRetry: Bool
}

extension ThreePartError {
    /// Builds the words for one engine error. `canRetry` is decided by the
    /// screen, because only the screen knows whether a retry control makes
    /// sense there.
    init(_ error: FerryError, canRetry: Bool) {
        var code = ""
        var detail: String?
        switch error {
        case let .Failed(errorCode, errorDetail):
            code = errorCode
            detail = errorDetail
        }

        if let words = FerryErrors.words(for: code) {
            self.init(
                whatStopped: FerryErrors.fill(words.stopped, detail: detail),
                why: FerryErrors.fill(words.why, detail: detail),
                whatToDo: FerryErrors.fill(words.todo, detail: detail),
                canRetry: canRetry
            )
        } else {
            // The table is generated from the same list of codes the engine
            // raises, so this should not happen. If it does, the code
            // itself is shown, because a guess would be worse.
            self.init(
                whatStopped: S.common.unknownErrorStopped,
                why: S.common.unknownErrorWhy(code: code),
                whatToDo: S.common.unknownErrorToDo,
                canRetry: canRetry
            )
        }
    }

    /// Turns any thrown error into three parts. A FerryError keeps its own
    /// words; anything else gets the general ones.
    static func from(_ error: Error, canRetry: Bool) -> ThreePartError {
        if let ferryError = error as? FerryError {
            return ThreePartError(ferryError, canRetry: canRetry)
        }
        if let keyError = error as? KeyStoreError {
            return keyError.threePart(canRetry: canRetry)
        }
        if let mountError = error as? FinderMountError {
            return mountError.threePart(canRetry: canRetry)
        }
        if let dropError = error as? DropError {
            return dropError.threePart(canRetry: canRetry)
        }
        return ThreePartError(
            whatStopped: S.common.unknownErrorStopped,
            why: S.common.unknownErrorWhy(code: String(describing: type(of: error))),
            whatToDo: S.common.unknownErrorToDo,
            canRetry: canRetry
        )
    }
}
