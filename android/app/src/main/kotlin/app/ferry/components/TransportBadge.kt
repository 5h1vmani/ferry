package app.ferry.components

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withStyle
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.model.ConnectionState
import app.ferry.model.Transport

// The words TransportBadge's screen reader description says, in
// docs/components.md's table. DeviceRow reuses this so the phrase is not
// written twice, once here and once in its own row description.
@Composable
fun transportBadgeContentDescription(transport: Transport, state: ConnectionState): String =
    when (state) {
        is ConnectionState.Idle -> if (transport == Transport.Usb) {
            stringResource(R.string.cd_transport_usb_idle)
        } else {
            stringResource(R.string.cd_transport_wifi_idle)
        }
        is ConnectionState.Moving -> if (transport == Transport.Usb) {
            stringResource(R.string.cd_transport_usb_moving, state.speedMBps)
        } else {
            stringResource(R.string.cd_transport_wifi_moving, state.speedMBps)
        }
        is ConnectionState.Connecting -> stringResource(R.string.cd_transport_connecting)
        is ConnectionState.NotReachable -> stringResource(R.string.cd_transport_not_reachable)
    }

// Says how a device is reachable right now. Appears on every DeviceRow and
// on every active transfer. docs/components.md.
@Composable
fun TransportBadge(
    transport: Transport,
    state: ConnectionState,
    modifier: Modifier = Modifier,
) {
    val notReachable = state is ConnectionState.NotReachable
    val color = if (notReachable) FerryColor.textSecondary() else FerryColor.text()

    val icon = if (notReachable) {
        FerryIcon.notReachable
    } else if (transport == Transport.Usb) {
        FerryIcon.usb
    } else {
        FerryIcon.wifi
    }

    val transportLabel = if (transport == Transport.Usb) {
        stringResource(R.string.transport_usb)
    } else {
        stringResource(R.string.transport_wifi)
    }

    val contentDesc = transportBadgeContentDescription(transport, state)

    // Label type for the transport name, mono for the speed, so the digits
    // in "38 MB/s" line up the way docs/components.md asks. stringResource
    // is @Composable, so every string is read before buildAnnotatedString,
    // whose builder lambda is not itself a composable scope.
    val labelStyle = FerryFont.label()
    val monoStyle = FerryFont.mono()
    val connectingLabel = stringResource(R.string.transport_connecting)
    val notReachableLabel = stringResource(R.string.transport_not_reachable)
    val speedText = if (state is ConnectionState.Moving) {
        stringResource(R.string.transport_speed_value, state.speedMBps)
    } else {
        null
    }
    val text = buildAnnotatedString {
        when (state) {
            is ConnectionState.Idle -> {
                withStyle(labelStyle.toSpanStyle()) { append(transportLabel) }
            }
            is ConnectionState.Moving -> {
                withStyle(labelStyle.toSpanStyle()) { append(transportLabel) }
                withStyle(labelStyle.toSpanStyle()) { append(" · ") }
                withStyle(monoStyle.toSpanStyle()) { append(speedText.orEmpty()) }
            }
            is ConnectionState.Connecting -> {
                withStyle(labelStyle.toSpanStyle()) { append(connectingLabel) }
            }
            is ConnectionState.NotReachable -> {
                withStyle(labelStyle.toSpanStyle()) { append(notReachableLabel) }
            }
        }
    }

    Row(
        modifier = modifier.semantics { contentDescription = contentDesc },
    ) {
        Icon(
            imageVector = ferryIconFor(icon),
            contentDescription = null,
            tint = color,
        )
        Spacer(Modifier.width(FerrySpace.s1))
        Text(text = text, color = color)
    }
}
