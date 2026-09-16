package app.ferry

import app.ferry.model.Origin
import java.util.Locale

// The plain logic behind the four transfer lines TransferRow draws and
// ReachableService repeats in a notification. Neither Compose's
// stringResource nor a Service's getString is called here: each caller
// resolves its own words, formats its own numbers, and hands the results
// to the function here that assembles them into one line.

// "Automatic · Phone to Mac", or just the direction. `direction` is
// already the resolved word for group.direction; `automaticLabel` and
// `dotSeparator` are read only when origin is Automatic.
fun buildDirectionLine(
    origin: Origin,
    direction: String,
    automaticLabel: String,
    dotSeparator: String,
): String {
    if (origin != Origin.Automatic) {
        return direction
    }
    return automaticLabel + dotSeparator + direction
}

// "43 of 120 files · 2.1 GB remaining · 38 MB/s". `filesProgress` and
// `speed` are null where the caller left that part out; `remaining` is
// always shown.
fun buildActiveLine(
    filesProgress: String?,
    remaining: String,
    speed: String?,
    dotSeparator: String,
): String = listOfNotNull(filesProgress, remaining, speed).joinToString(dotSeparator)

// "Paused." alone, or "Paused." with why and what to do after it.
// `pausedTemplate` is the raw, unfilled resource string, since the reason
// is computed here from `why` and `todo`, and filled into it the same way
// Android's own getString(id, args) fills a template.
fun buildPausedLine(
    code: String?,
    why: String?,
    todo: String?,
    pausedPlain: String,
    pausedTemplate: String,
): String {
    if (code == null) {
        return pausedPlain
    }
    val reason = listOfNotNull(why, todo).joinToString(" ")
    return String.format(Locale.getDefault(), pausedTemplate, reason)
}

// "12 files · 4.8 GB · 3 min". `fileCount` and `duration` are null where
// the caller left that part out; `size` is always shown.
fun buildDoneLine(
    fileCount: String?,
    size: String,
    duration: String?,
    dotSeparator: String,
): String = listOfNotNull(fileCount, size, duration).joinToString(dotSeparator)
