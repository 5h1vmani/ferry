package app.ferry

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import app.ferry.model.ThreePartError
import uniffi.ferry_runtime.FerryException

// Turns an engine error code into the three parts an ErrorBlock draws.
//
// Every error a person sees passes through here. The words come from the
// generated table in Errors.kt, which is built from design/errors.json, so
// the same code says the same thing on the phone and on the Mac.

// The lookup, fallback, fill, and trim behind every three-part error a
// person sees. threePartError below reads the unknown-code words through
// stringResource, for a Compose screen; errorWordsFor further below reads
// the same words through getString, for a caller with no Compose context.
// Each caller resolves its own three words and hands them here.
//
// A part the table leaves empty is dropped, never guessed, which is the
// rule in docs/voice.md.
fun buildThreePartError(
    code: String,
    detail: String?,
    unknownStopped: String,
    unknownWhy: String,
    unknownTodo: String,
): ThreePartError {
    val words = FerryErrors.wordsFor(code)
        ?: return ThreePartError(stopped = unknownStopped, why = unknownWhy, todo = unknownTodo)
    return ThreePartError(
        stopped = FerryErrors.fill(words.stopped, detail).trim(),
        why = FerryErrors.fill(words.why, detail).trim().ifEmpty { null },
        todo = FerryErrors.fill(words.todo, detail).trim().ifEmpty { null },
    )
}

// The three parts for one code, read for a Compose screen.
@Composable
fun threePartError(code: String, detail: String? = null): ThreePartError = buildThreePartError(
    code = code,
    detail = detail,
    unknownStopped = stringResource(R.string.error_unknown_stopped),
    unknownWhy = stringResource(R.string.error_unknown_why, code),
    unknownTodo = stringResource(R.string.error_unknown_todo),
)

// The three parts for an error the engine threw.
@Composable
fun threePartError(error: FerryException): ThreePartError {
    val failure = error as? FerryException.Failed
    return threePartError(failure?.code.orEmpty(), failure?.detail)
}

// The plain-Context twin of threePartError above, for a caller with no
// Compose context: ReachableService and TransferNotifier, which read the
// unknown-code words through getString. Returns FerryErrors.Words, whose
// why and todo are never null, so a dropped part reads as an empty string
// rather than as null.
fun errorWordsFor(context: Context, code: String, detail: String?): FerryErrors.Words {
    val words = buildThreePartError(
        code = code,
        detail = detail,
        unknownStopped = context.getString(R.string.error_unknown_stopped),
        unknownWhy = context.getString(R.string.error_unknown_why, code),
        unknownTodo = context.getString(R.string.error_unknown_todo),
    )
    return FerryErrors.Words(
        stopped = words.stopped,
        why = words.why.orEmpty(),
        todo = words.todo.orEmpty(),
    )
}
