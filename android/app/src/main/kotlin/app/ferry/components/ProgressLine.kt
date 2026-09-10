package app.ferry.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.formatSize
import app.ferry.model.TransferInfo
import app.ferry.model.TransferState
import app.ferry.threePartError

// One transfer's progress, in one line. docs/components.md. Failed has no
// line of its own: an ErrorBlock takes its place.
//
// The engine reports one file per transfer, with a byte count, so the
// active line counts bytes. docs/components.md writes "3 of 120 files",
// which describes a whole folder; the engine has no such record, and this
// line does not invent one.
//
// The "Transferring, N percent" announcement docs/components.md asks for is
// meant to update no more than once every 5 seconds. The engine already
// rations transfersChanged to once every 250 milliseconds, which is faster
// than that, so a screen reader throttle is still owed. It is not built
// here, because the phone does not pull in phase 1 and this line has
// nothing to announce yet.
@Composable
fun ProgressLine(
    transfer: TransferInfo,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (transfer.state) {
        TransferState.Queued -> {
            val line = stringResource(R.string.progress_queued)
            Text(text = line, style = FerryFont.mono(), modifier = modifier)
        }

        TransferState.Active -> {
            val done = formatSize(transfer.bytesDone)
            val total = formatSize(transfer.bytesTotal)
            val line = stringResource(R.string.progress_active, done, total)
            val fraction = if (transfer.bytesTotal > 0L) {
                transfer.bytesDone.toFloat() / transfer.bytesTotal.toFloat()
            } else {
                0f
            }
            val percent = (fraction * 100f).toInt()
            val transferringDescription =
                stringResource(R.string.cd_progress_transferring_percent, percent)
            Column(
                modifier = modifier.semantics {
                    contentDescription = "$line. $transferringDescription"
                },
            ) {
                LinearProgressIndicator(
                    progress = { fraction },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(4.dp),
                    color = FerryColor.accent(),
                    trackColor = FerryColor.border(),
                )
                Spacer(Modifier.height(FerrySpace.s1))
                Text(text = line, style = FerryFont.mono())
            }
        }

        TransferState.Paused -> {
            // The reason is the why and the what to do from the error
            // table. A pause the engine gave no code for says only that it
            // is paused, because the rest is not known.
            val code = transfer.errorCode
            val line = if (code == null) {
                stringResource(R.string.progress_paused_plain)
            } else {
                val words = threePartError(code, transfer.errorDetail)
                val reason = listOfNotNull(words.why, words.todo).joinToString(" ")
                stringResource(R.string.progress_paused, reason)
            }
            Column(modifier = modifier.semantics { contentDescription = line }) {
                LinearProgressIndicator(
                    progress = { 1f },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(4.dp),
                    color = FerryColor.borderStrong(),
                    trackColor = FerryColor.borderStrong(),
                )
                Spacer(Modifier.height(FerrySpace.s1))
                Text(text = line, style = FerryFont.mono())
            }
        }

        TransferState.Done -> {
            // The size only. docs/components.md also shows how long it
            // took, and the engine keeps no duration, so that part is left
            // out rather than guessed.
            val line = formatSize(transfer.bytesTotal)
            Row(modifier = modifier.semantics { contentDescription = line }) {
                Icon(
                    imageVector = ferryIconFor(FerryIcon.done),
                    contentDescription = null,
                    tint = FerryColor.text(),
                )
                Spacer(Modifier.width(FerrySpace.s1))
                Text(text = line, style = FerryFont.mono())
            }
        }

        TransferState.Failed -> {
            val code = transfer.errorCode
            if (code != null) {
                ErrorBlock(
                    error = threePartError(code, transfer.errorDetail),
                    onAction = onRetry,
                    modifier = modifier,
                )
            }
        }
    }
}
