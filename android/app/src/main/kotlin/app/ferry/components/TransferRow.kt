package app.ferry.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.buildActiveLine
import app.ferry.buildDirectionLine
import app.ferry.buildDoneLine
import app.ferry.buildPausedLine
import app.ferry.formatDuration
import app.ferry.formatSize
import app.ferry.model.Direction
import app.ferry.model.TransferGroup
import app.ferry.model.TransferState
import app.ferry.threePartError

// One group of transfers as one row of the Transfers section.
// docs/ia.md, Transfers.
//
// A batch and a single transfer draw the same way, so this view never asks
// which it has: a folder copy of 120 files is one row, and so is one pull.
// model/Mapping.kt folds both into TransferGroup.
//
// The row states four things in a fixed order: what is moving, where it
// came from, how far it has got, and — only where a chunk fact exists —
// how far down a failure goes.
//
// The bar advances linearly. An easing curve would be a small lie about
// throughput.
@Composable
fun TransferRow(
    group: TransferGroup,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.padding(vertical = FerrySpace.s2)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = group.label,
                style = FerryFont.body(),
                color = FerryColor.text(),
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(FerrySpace.s2))
            Text(
                text = originAndDirection(group),
                style = FerryFont.caption(),
                color = FerryColor.textSecondary(),
            )
        }

        Spacer(Modifier.height(FerrySpace.s1))
        GroupProgress(group = group, onRetry = onRetry)

        val chunks = group.chunks
        if (chunks != null && group.state == TransferState.Failed) {
            ChunkDisclosure(chunks = chunks)
        }
    }
}

// "Automatic · Phone to Mac", or just the direction.
//
// Origin is a word in the same type as the direction, because it is the
// same kind of fact: how this transfer came to exist. Job 7's copies would
// otherwise be indistinguishable from ones a person asked for.
@Composable
private fun originAndDirection(group: TransferGroup): String {
    val direction = when (group.direction) {
        Direction.Pull -> stringResource(R.string.transfers_direction_mac_to_phone)
        Direction.Push -> stringResource(R.string.transfers_direction_phone_to_mac)
    }
    return buildDirectionLine(
        origin = group.origin,
        direction = direction,
        automaticLabel = stringResource(R.string.transfers_origin_automatic),
        dotSeparator = stringResource(R.string.dot_separator),
    )
}

@Composable
private fun GroupProgress(group: TransferGroup, onRetry: () -> Unit) {
    when (group.state) {
        TransferState.Queued -> Text(
            text = stringResource(R.string.progress_queued),
            style = FerryFont.mono(),
            color = FerryColor.textSecondary(),
        )

        TransferState.Active -> {
            val line = activeLine(group)
            val percent = stringResource(
                R.string.cd_progress_transferring_percent,
                group.percent,
            )
            Column(modifier = Modifier.semantics { contentDescription = "$line. $percent" }) {
                LinearProgressIndicator(
                    progress = { group.fraction },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(FerrySpace.s1),
                    color = FerryColor.accent(),
                    trackColor = FerryColor.border(),
                )
                Spacer(Modifier.height(FerrySpace.s1))
                Text(text = line, style = FerryFont.mono(), color = FerryColor.text())
            }
        }

        TransferState.Paused -> {
            // The bar stops and turns grey. Never red, and no retry
            // control: the engine resumes it on its own when the device is
            // reachable again.
            val line = pausedLine(group)
            Column(modifier = Modifier.semantics { contentDescription = line }) {
                LinearProgressIndicator(
                    progress = { group.fraction },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(FerrySpace.s1),
                    color = FerryColor.borderStrong(),
                    trackColor = FerryColor.border(),
                )
                Spacer(Modifier.height(FerrySpace.s1))
                Text(text = line, style = FerryFont.body(), color = FerryColor.text())
            }
        }

        TransferState.Done -> {
            val line = doneLine(group)
            Row(
                modifier = Modifier.semantics { contentDescription = line },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    imageVector = ferryIconFor(FerryIcon.done),
                    contentDescription = null,
                    tint = FerryColor.text(),
                )
                Spacer(Modifier.width(FerrySpace.s1))
                Text(text = line, style = FerryFont.mono(), color = FerryColor.text())
            }
        }

        TransferState.Failed -> {
            val code = group.errorCode
            if (code != null) {
                ErrorBlock(
                    error = threePartError(code, group.errorDetail),
                    onAction = onRetry,
                )
            }
        }
    }
}

// "43 of 120 files · 2.1 GB remaining · 38 MB/s".
//
// Each part that is unknown is left out, not guessed: a single file states
// no file count, and a group with no reported speed states no speed. That
// is the three-part rule applied to a progress line.
@Composable
private fun activeLine(group: TransferGroup): String {
    val filesProgress = if (!group.isSingleFile) {
        stringResource(R.string.transfers_files_progress, group.filesDone, group.filesTotal)
    } else {
        null
    }
    val remaining = (group.bytesTotal - group.bytesDone).coerceAtLeast(0L)
    val remainingText = stringResource(R.string.progress_remaining, formatSize(remaining))
    val speed = group.speedMBps
    val speedText = if (speed != null && speed > 0) {
        stringResource(R.string.transport_speed_value, speed)
    } else {
        null
    }
    return buildActiveLine(filesProgress, remainingText, speedText, stringResource(R.string.dot_separator))
}

// "Paused." and then why and what to do, from the error table. A pause the
// engine gave no code for says only that it is paused, because the rest is
// not known.
@Composable
private fun pausedLine(group: TransferGroup): String {
    val code = group.errorCode
    val words = code?.let { threePartError(it, group.errorDetail) }
    return buildPausedLine(
        code = code,
        why = words?.why,
        todo = words?.todo,
        pausedPlain = stringResource(R.string.progress_paused_plain),
        pausedTemplate = stringResource(R.string.progress_paused),
    )
}

// "12 files · 4.8 GB · 3 min".
@Composable
private fun doneLine(group: TransferGroup): String {
    val fileCount = if (!group.isSingleFile) {
        stringResource(R.string.transfers_file_count, group.filesTotal)
    } else {
        null
    }
    val size = formatSize(group.bytesTotal)
    val duration = group.durationSecs?.let { formatDuration(it) }
    return buildDoneLine(fileCount, size, duration, stringResource(R.string.dot_separator))
}
