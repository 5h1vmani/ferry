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
import app.ferry.model.TransferInfo
import app.ferry.model.TransferState

// One transfer's progress, in one line. docs/components.md. Failed has no
// line of its own: an ErrorBlock takes its place.
//
// The "Transferring, N percent" announcement docs/components.md asks for is
// meant to update no more than once every 5 seconds, so it does not talk
// over itself. That throttle belongs to whatever feeds live progress in
// later, once the core is linked; sample state never changes, so there is
// nothing to throttle here yet.
@Composable
fun ProgressLine(
    transfer: TransferInfo,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (transfer.state) {
        TransferState.Active -> {
            val done = transfer.filesDone ?: 0
            val total = transfer.filesTotal ?: 0
            val speedText = transfer.speedMBps?.let {
                stringResource(R.string.transport_speed_value, it)
            }.orEmpty()
            val line = stringResource(
                R.string.progress_active,
                done,
                total,
                transfer.bytesRemainingText.orEmpty(),
                speedText,
            )
            val percent = if (total > 0) (done * 100) / total else 0
            val transferringDescription = stringResource(R.string.cd_progress_transferring_percent, percent)
            Column(modifier = modifier.semantics { contentDescription = "$line. $transferringDescription" }) {
                LinearProgressIndicator(
                    progress = { if (total > 0) done.toFloat() / total.toFloat() else 0f },
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
            val line = stringResource(R.string.progress_paused, transfer.pausedReason.orEmpty())
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
            val line = stringResource(
                R.string.progress_done,
                transfer.doneFileCount ?: 0,
                transfer.doneSizeText.orEmpty(),
                transfer.doneDurationText.orEmpty(),
            )
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
            val error = transfer.error
            if (error != null) {
                ErrorBlock(error = error, onRetry = onRetry, modifier = modifier)
            }
        }
    }
}
