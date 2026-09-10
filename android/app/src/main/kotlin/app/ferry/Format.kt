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

// A unix time as a date in the person's own language and order. No Ferry
// string is involved, so this one needs no template.
fun formatDate(unixSecs: Long): String {
    val date = Instant.ofEpochSecond(unixSecs).atZone(ZoneId.systemDefault()).toLocalDate()
    return DateTimeFormatter.ofLocalizedDate(FormatStyle.LONG).format(date)
}
