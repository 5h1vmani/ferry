package app.ferry

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

// A byte count as a short decimal size: "512 B", "4.8 kB", "2.1 GB". The
// units are decimal, not binary, because docs/voice.md writes "2.1 GB" and
// "4.8 GB" the way a disk and a network do.
@Composable
fun formatSize(bytes: Long): String {
    if (bytes < BYTES_PER_KILOBYTE) {
        return stringResource(R.string.size_bytes, bytes)
    }
    var value = bytes.toDouble() / BYTES_PER_KILOBYTE
    val templates = listOf(
        R.string.size_kilobytes,
        R.string.size_megabytes,
        R.string.size_gigabytes,
        R.string.size_terabytes,
    )
    var index = 0
    while (value >= BYTES_PER_KILOBYTE && index < templates.size - 1) {
        value /= BYTES_PER_KILOBYTE
        index += 1
    }
    val number = String.format(Locale.getDefault(), "%.1f", value)
    return stringResource(templates[index], number)
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

// How long a finished transfer took: "18 s", "3 min", "1 h 12 min". The
// engine now carries both timestamps, so a done row states its duration
// instead of leaving it out.
@Composable
fun formatDuration(seconds: Long): String {
    val clamped = seconds.coerceAtLeast(0L)
    if (clamped < SECONDS_PER_MINUTE) {
        return stringResource(R.string.duration_seconds, clamped)
    }
    val minutes = clamped / SECONDS_PER_MINUTE
    if (minutes < SECONDS_PER_MINUTE) {
        return stringResource(R.string.duration_minutes_short, minutes)
    }
    return stringResource(
        R.string.duration_hours_short,
        minutes / SECONDS_PER_MINUTE,
        minutes % SECONDS_PER_MINUTE,
    )
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
