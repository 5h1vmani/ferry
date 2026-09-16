package app.ferry

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

// Numbers turned into the text a person reads. Every template lives in
// strings.xml, so no sentence is written in a view.
//
// Every number any screen shows passes through this file. That is the
// point: two views that print a byte count must print it the same way, and
// a change to how Ferry rounds is a change in one place.

private const val BYTES_PER_KILOBYTE = 1_000.0
private const val SECONDS_PER_MINUTE = 60L
private const val SECONDS_PER_HOUR = 3_600L
private const val SECONDS_PER_DAY = 86_400L

// Which size template a byte count needs, and the number to put in it. The
// arithmetic behind formatSize, shared by its two overloads below, neither
// of which reads a resource itself.
private sealed class SizeWords {
    // Under one kilobyte: shown as a whole number of bytes.
    data class Bytes(val bytes: Long) : SizeWords()

    // One kilobyte or more: shown to one decimal place, at the largest
    // unit where the value is still under a thousand.
    data class Scaled(val templateIndex: Int, val number: String) : SizeWords()
}

// The templates a Scaled result's index picks between, in the order the
// arithmetic below counts them.
private val SIZE_TEMPLATES = listOf(
    R.string.size_kilobytes,
    R.string.size_megabytes,
    R.string.size_gigabytes,
    R.string.size_terabytes,
)

private fun sizeWordsFor(bytes: Long): SizeWords {
    if (bytes < BYTES_PER_KILOBYTE) {
        return SizeWords.Bytes(bytes)
    }
    var value = bytes.toDouble() / BYTES_PER_KILOBYTE
    var index = 0
    while (value >= BYTES_PER_KILOBYTE && index < SIZE_TEMPLATES.size - 1) {
        value /= BYTES_PER_KILOBYTE
        index += 1
    }
    val number = String.format(Locale.getDefault(), "%.1f", value)
    return SizeWords.Scaled(index, number)
}

// A byte count as a short decimal size: "512 B", "4.8 kB", "2.1 GB". The
// units are decimal, not binary, because docs/voice.md writes "2.1 GB" and
// "4.8 GB" the way a disk and a network do.
@Composable
fun formatSize(bytes: Long): String = when (val words = sizeWordsFor(bytes)) {
    is SizeWords.Bytes -> stringResource(R.string.size_bytes, words.bytes)
    is SizeWords.Scaled -> stringResource(SIZE_TEMPLATES[words.templateIndex], words.number)
}

// The same rule as the Composable formatSize above, for a caller with no
// Compose context: ReachableService, building a transfer notification's
// text off the main thread. The templates are the same strings.xml
// entries, read through Context.getString instead of stringResource.
fun formatSize(context: Context, bytes: Long): String = when (val words = sizeWordsFor(bytes)) {
    is SizeWords.Bytes -> context.getString(R.string.size_bytes, words.bytes)
    is SizeWords.Scaled -> context.getString(SIZE_TEMPLATES[words.templateIndex], words.number)
}

// How long ago a moment was, as one number and one unit: "2 hours".
@Composable
fun formatAgo(unixSecs: Long): String {
    val seconds = (System.currentTimeMillis() / 1000L) - unixSecs
    if (seconds < SECONDS_PER_MINUTE) {
        return stringResource(R.string.duration_under_a_minute)
    }
    if (seconds < SECONDS_PER_HOUR) {
        val minutes = (seconds / SECONDS_PER_MINUTE).toInt()
        return pluralStringResource(R.plurals.duration_minutes, minutes, minutes)
    }
    if (seconds < SECONDS_PER_DAY) {
        val hours = (seconds / SECONDS_PER_HOUR).toInt()
        return pluralStringResource(R.plurals.duration_hours, hours, hours)
    }
    val days = (seconds / SECONDS_PER_DAY).toInt()
    return pluralStringResource(R.plurals.duration_days, days, days)
}

// Which duration template a second count needs, and its numbers. The
// arithmetic behind formatDuration, shared by its two overloads below,
// neither of which reads a resource itself.
private sealed class DurationWords {
    data class Seconds(val seconds: Long) : DurationWords()
    data class MinutesShort(val minutes: Long) : DurationWords()
    data class HoursShort(val hours: Long, val minutes: Long) : DurationWords()
}

private fun durationWordsFor(seconds: Long): DurationWords {
    val clamped = seconds.coerceAtLeast(0L)
    if (clamped < SECONDS_PER_MINUTE) {
        return DurationWords.Seconds(clamped)
    }
    val minutes = clamped / SECONDS_PER_MINUTE
    if (minutes < SECONDS_PER_MINUTE) {
        return DurationWords.MinutesShort(minutes)
    }
    return DurationWords.HoursShort(minutes / SECONDS_PER_MINUTE, minutes % SECONDS_PER_MINUTE)
}

// How long a finished transfer took: "18 s", "3 min", "1 h 12 min". The
// engine now carries both timestamps, so a done row states its duration
// instead of leaving it out.
@Composable
fun formatDuration(seconds: Long): String = when (val words = durationWordsFor(seconds)) {
    is DurationWords.Seconds -> stringResource(R.string.duration_seconds, words.seconds)
    is DurationWords.MinutesShort -> stringResource(R.string.duration_minutes_short, words.minutes)
    is DurationWords.HoursShort ->
        stringResource(R.string.duration_hours_short, words.hours, words.minutes)
}

// The same rule as the Composable formatDuration above, for ReachableService.
fun formatDuration(context: Context, seconds: Long): String = when (val words = durationWordsFor(seconds)) {
    is DurationWords.Seconds -> context.getString(R.string.duration_seconds, words.seconds)
    is DurationWords.MinutesShort -> context.getString(R.string.duration_minutes_short, words.minutes)
    is DurationWords.HoursShort ->
        context.getString(R.string.duration_hours_short, words.hours, words.minutes)
}

// The time a pairing code has left, as "1:12". Counted, not described,
// because "soon" is an adjective standing in for a number.
//
// No template: a clock count is digits and a colon in every language this
// app will ship in, and putting it in strings.xml would invite a
// translation that is not one.
fun formatCountdown(seconds: Long): String {
    val clamped = seconds.coerceAtLeast(0L)
    return String.format(
        Locale.getDefault(),
        "%d:%02d",
        clamped / SECONDS_PER_MINUTE,
        clamped % SECONDS_PER_MINUTE,
    )
}

// A moment as a clock time: "14:31", or "2:31 PM" where that is the
// person's own format. One access log row's time.
fun formatTimeOfDay(unixSecs: Long): String {
    val time = Instant.ofEpochSecond(unixSecs).atZone(ZoneId.systemDefault()).toLocalTime()
    return DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT).format(time)
}

// A unix time as a date in the person's own language and order. No Ferry
// string is involved, so this one needs no template.
fun formatDate(unixSecs: Long): String {
    val date = Instant.ofEpochSecond(unixSecs).atZone(ZoneId.systemDefault()).toLocalDate()
    return DateTimeFormatter.ofLocalizedDate(FormatStyle.LONG).format(date)
}
