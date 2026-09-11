package app.ferry.components

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Icon
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
import app.ferry.FerryIcon
import app.ferry.R
import app.ferry.formatAgo
import app.ferry.model.ConnectionState
import app.ferry.model.DeviceInfo
import app.ferry.model.DeviceKind
import app.ferry.model.Transport

// One paired device in the Devices list. docs-v2/components.md, DeviceRow.
//
// The row is not a link. A phone holds one Mac, so a tap that selects it
// changes nothing: there is no device detail screen, and the transfers sit
// under this row on the same screen. docs-v2/ia.md, On the phone.
//
// Screen reader says the name, then the badge text, then the spare
// transport if there is one, then last seen if present, as one
// announcement. TransportBadge sets its own contentDescription, so merging
// descendants would read its text a second time; clearAndSetSemantics
// replaces the whole subtree with exactly the one string
// docs-v2/components.md asks for.
@Composable
fun DeviceRow(
    device: DeviceInfo,
    modifier: Modifier = Modifier,
) {
    val notReachable = device.connectionState is ConnectionState.NotReachable
    val iconColor = if (notReachable) FerryColor.textSecondary() else FerryColor.text()

    val badgeDescription =
        transportBadgeContentDescription(device.transport, device.connectionState)

    // A transport that is available but is not carrying bytes, stated once
    // beside the badge so a pulled cable is not a surprise. It is not a
    // state of the badge: a badge that names two paths stops answering
    // which one is carrying this.
    val spare = device.spareTransport
    val spareLine = if (spare != null) {
        stringResource(
            when (spare) {
                Transport.Usb -> R.string.devices_spare_transport_usb
                Transport.Wifi -> R.string.devices_spare_transport_wifi
            },
        )
    } else {
        null
    }

    // The engine reports when the device was last reachable as a unix time.
    // The row turns it into "2 hours" and places that in the template.
    val lastSeen = device.lastSeenUnixSecs
    val lastSeenLine = if (notReachable && lastSeen != null) {
        stringResource(R.string.device_last_seen, formatAgo(lastSeen))
    } else {
        null
    }

    val rowDescription =
        listOfNotNull(device.name, badgeDescription, spareLine, lastSeenLine).joinToString(". ")

    ListItem(
        modifier = modifier.clearAndSetSemantics { contentDescription = rowDescription },
        headlineContent = { Text(text = device.name, style = FerryFont.body()) },
        supportingContent = {
            Column {
                TransportBadge(transport = device.transport, state = device.connectionState)
                if (spareLine != null) {
                    Text(
                        text = spareLine,
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
                if (lastSeenLine != null) {
                    Text(
                        text = lastSeenLine,
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
            }
        },
        leadingContent = {
            Icon(
                // The engine sends the peer's kind in hello, so the row
                // draws the device's own icon rather than assuming every
                // peer of a phone is a Mac.
                imageVector = ferryIconFor(
                    when (device.kind) {
                        DeviceKind.Mac -> FerryIcon.deviceMac
                        DeviceKind.Phone -> FerryIcon.devicePhone
                    },
                ),
                contentDescription = null,
                tint = iconColor,
            )
        },
        colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
    )
}
