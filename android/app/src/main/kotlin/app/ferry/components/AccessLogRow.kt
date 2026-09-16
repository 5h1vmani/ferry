package app.ferry.components

import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.R
import app.ferry.formatSize
import app.ferry.formatTimeOfDay
import app.ferry.model.AccessActor
import app.ferry.model.AccessEntry
import app.ferry.model.AccessVerb

// One file operation, as a sentence. docs/components.md, AccessLogRow.
// L5, job 9.
//
// The subject is always named — "MacBook Pro read" or "This phone read" —
// never "you" and never a direction icon. A log is read months later, out
// of context, and an arrow does not survive that.
//
// The verbs are the file operations layer's own words, so a log line and a
// protocol trace say the same word.
//
// Nothing in the row is coloured and nothing in it is a control. A row
// states a fact, offers no judgment, and offers no action. Job 9 records
// what happened; it does not decide that something was wrong.
@Composable
fun AccessLogRow(
    entry: AccessEntry,
    // The paired device's name, for the sentence's subject. The row does
    // not know which device it belongs to; it is told.
    peerName: String,
    modifier: Modifier = Modifier,
) {
    val subject = when (entry.actor) {
        AccessActor.Peer -> peerName
        AccessActor.ThisDevice -> stringResource(R.string.access_log_this_phone)
    }
    val verb = stringResource(verbLabel(entry.verb))
    val files = entry.files
    val sentence = if (files != null) {
        stringResource(R.string.access_log_sentence_files, subject, verb, entry.path, files)
    } else {
        stringResource(R.string.access_log_sentence, subject, verb, entry.path)
    }

    val time = formatTimeOfDay(entry.atUnixSecs)
    val bytes = entry.bytes
    val entries = entry.entries
    val amount = when {
        bytes != null -> formatSize(bytes)
        entries != null -> stringResource(R.string.access_log_entries, entries)
        else -> null
    }
    val supporting = listOfNotNull(time, amount).joinToString(stringResource(R.string.dot_separator))

    // The sentence first, because it is what a person is looking for, then
    // the numbers that qualify it.
    val description = listOfNotNull(sentence, time, amount).joinToString(". ")

    ListItem(
        modifier = modifier.clearAndSetSemantics { contentDescription = description },
        headlineContent = { Text(text = sentence, style = FerryFont.body()) },
        supportingContent = {
            Text(
                text = supporting,
                style = FerryFont.mono(),
                color = FerryColor.textSecondary(),
            )
        },
        colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
    )
}

// The word for each verb. One table, so a verb is never named twice.
private fun verbLabel(verb: AccessVerb): Int = when (verb) {
    AccessVerb.List -> R.string.access_verb_list
    AccessVerb.Stat -> R.string.access_verb_stat
    AccessVerb.Read -> R.string.access_verb_read
    AccessVerb.Write -> R.string.access_verb_write
    AccessVerb.Truncate -> R.string.access_verb_truncate
    AccessVerb.Rename -> R.string.access_verb_rename
    AccessVerb.Mkdir -> R.string.access_verb_mkdir
    AccessVerb.Delete -> R.string.access_verb_delete
}
