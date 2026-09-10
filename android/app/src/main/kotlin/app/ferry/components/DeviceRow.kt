package app.ferry.components

import androidx.compose.foundation.clickable
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
import androidx.compose.ui.semantics.onClick
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.R
import app.ferry.formatAgo
import app.ferry.model.ConnectionState
import app.ferry.model.DeviceInfo

// One paired Mac in the Devices list. docs/components.md.
//
// Screen reader says the name, then the badge text, then last seen if
// present, as one announcement. TransportBadge sets its own
// contentDescription, so merging descendants would read its text a second
// time; clearAndSetSemantics replaces the whole subtree with exactly the
// one string docs/components.md asks for, and restates the click action
// clickable() would otherwise have contributed.
@Composable
fun DeviceRow(
    device: DeviceInfo,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val notReachable = device.connectionState is ConnectionState.NotReachable
    val iconColor = if (notReachable) FerryColor.textSecondary() else FerryColor.text()

    val badgeDescription = transportBadgeContentDescription(device.transport, device.connectionState)
    // The engine reports when the device was last reachable as a unix time.
    // The row turns it into "2 hours" and places that in the template.
    val lastSeen = device.lastSeenUnixSecs
    val lastSeenLine = if (notReachable && lastSeen != null) {
        stringResource(R.string.device_last_seen, formatAgo(lastSeen))
    } else {
        null
    }
    val rowDescription = listOfNotNull(device.name, badgeDescription, lastSeenLine)
        .joinToString(". ")

    ListItem(
        modifier = modifier
            .clickable(onClick = onClick)
            .clearAndSetSemantics {
                contentDescription = rowDescription
                onClick(action = { onClick(); true })
            },
        headlineContent = { Text(text = device.name, style = FerryFont.body()) },
        supportingContent = {
            Column {
                TransportBadge(transport = device.transport, state = device.connectionState)
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
                imageVector = ferryIconFor(FerryIcon.deviceMac),
                contentDescription = null,
                tint = iconColor,
            )
        },
        colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
    )
}
