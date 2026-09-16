package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.PairingCode
import app.ferry.components.ferryIconFor
import app.ferry.formatCountdown
import kotlinx.coroutines.delay

// PairingScreen's Waiting, Code, and Confirmed content, and the countdown
// both of the first two show. Split from PairingScreen.kt,
// docs/audits/principles.md row P13.
//
// PairingScreen still owns which of these draws, and every tap callback
// it passes in; nothing here reads FerryEngine directly.

// How long the paired icon stays before Devices comes back. Internal
// because PairingScreen's own LaunchedEffect for PairingStep.Confirmed
// reads it too.
internal const val CONFIRMED_MILLIS = 1_000L

private const val TICK_MILLIS = 1_000L

@Composable
fun WaitingContent(
    starting: Boolean,
    shortCode: String?,
    expiresUnixSecs: Long,
    onCancel: () -> Unit,
    onScan: () -> Unit,
) {
    Column {
        Text(
            text = stringResource(
                if (starting) R.string.pairing_starting else R.string.pairing_waiting_title,
            ),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
        Spacer(Modifier.height(FerrySpace.s4))
        if (shortCode != null) {
            // The only place this phone's random mDNS name is ever shown. A
            // person in a room with three phones can tell which is theirs,
            // and a wrong pick is safe anyway, because the six digits will
            // not match.
            Text(
                text = stringResource(R.string.pairing_short_code_is),
                style = FerryFont.body(),
                color = FerryColor.textSecondary(),
            )
            Text(
                text = shortCode,
                style = FerryFont.display(),
                color = FerryColor.text(),
            )
        }
        Spacer(Modifier.height(FerrySpace.s3))
        Countdown(expiresUnixSecs = expiresUnixSecs)
        Spacer(Modifier.height(FerrySpace.s5))
        TextButton(onClick = onCancel) {
            Text(stringResource(R.string.action_cancel))
        }
        Spacer(Modifier.height(FerrySpace.s2))
        AlternativeButton(
            label = stringResource(R.string.pairing_scan_instead),
            onClick = onScan,
        )
    }
}

// The code method's six digits, and the same countdown Waiting shows.
@Composable
fun CodeContent(
    digits: String,
    expiresUnixSecs: Long,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    PairingCode(
        code = digits,
        onConfirm = onConfirm,
        onCancel = onCancel,
    )
    Spacer(Modifier.height(FerrySpace.s3))
    Countdown(expiresUnixSecs = expiresUnixSecs)
}

// The two minute timeout, counted. Not "soon", which is an adjective
// standing in for a number.
//
// The engine publishes the deadline on every pairing state, so this counts
// down from that rather than from when the screen opened: a screen reopened
// mid-pairing shows the time that is actually left.
@Composable
fun Countdown(expiresUnixSecs: Long) {
    if (expiresUnixSecs <= 0L) {
        return
    }
    var now by remember { mutableLongStateOf(System.currentTimeMillis() / 1_000L) }
    LaunchedEffect(expiresUnixSecs) {
        while (true) {
            delay(TICK_MILLIS)
            now = System.currentTimeMillis() / 1_000L
        }
    }
    val remaining = (expiresUnixSecs - now).coerceAtLeast(0L)
    Text(
        text = stringResource(R.string.pairing_stops_in, formatCountdown(remaining)),
        style = FerryFont.caption(),
        color = FerryColor.textSecondary(),
    )
}

@Composable
fun ConfirmedContent() {
    Column {
        Icon(
            imageVector = ferryIconFor(FerryIcon.paired),
            contentDescription = null,
            tint = FerryColor.accent(),
            modifier = Modifier.size(FerrySpace.s7),
        )
        Spacer(Modifier.height(FerrySpace.s3))
        Text(
            text = stringResource(R.string.pairing_confirmed_label),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
    }
}
