package app.ferry.components

import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
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

// Whether this phone advertises and accepts connections, and what that
// costs when it does not. docs-v2/components.md, PresenceControl. Job 5's
// only control.
//
// It appears in two places on the phone: pinned under the top bar on
// Devices, and in the ongoing notification. Both read one value, which is
// why this is a component and not two views. The Mac has two more surfaces
// and reads the same value there.
//
// This holds no switch state of its own. A switch with local state and a
// remote truth drifts, and reachability is the one fact a person turns off
// in a hurry and then has to trust. The value comes from the engine's
// status() every time.
//
// It left Settings, where the first version of the IA filed it. It is a
// mode with a consequence, reached in a hotel lobby in a hurry, not a
// preference.
@Composable
fun PresenceControl(
    isAdvertising: Boolean,
    onChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    val label = if (isAdvertising) {
        stringResource(R.string.presence_advertising)
    } else {
        stringResource(R.string.presence_not_advertising)
    }
    // Stated because the failure it causes is silent: Wi-Fi transfers stop
    // working and a person who cannot see why has no way to guess.
    val consequence = stringResource(R.string.presence_consequence)
    val description = if (isAdvertising) {
        stringResource(R.string.cd_presence_on)
    } else {
        stringResource(R.string.cd_presence_off, consequence)
    }

    ListItem(
        modifier = modifier.clearAndSetSemantics { contentDescription = description },
        headlineContent = { Text(text = label, style = FerryFont.body()) },
        supportingContent = if (isAdvertising) {
            null
        } else {
            {
                Text(
                    text = consequence,
                    style = FerryFont.caption(),
                    color = FerryColor.textSecondary(),
                )
            }
        },
        leadingContent = {
            Icon(
                imageVector = ferryIconFor(
                    if (isAdvertising) FerryIcon.wifi else FerryIcon.advertisingOff,
                ),
                contentDescription = null,
                tint = FerryColor.text(),
            )
        },
        trailingContent = {
            Switch(
                checked = isAdvertising,
                onCheckedChange = onChange,
                colors = SwitchDefaults.colors(checkedTrackColor = FerryColor.accent()),
            )
        },
        colors = ListItemDefaults.colors(
            // Off, the row lifts out of the screen with one step of grey.
            // Never red: nothing dangerous happened, a person chose this.
            containerColor = if (isAdvertising) {
                FerryColor.accentSurface()
            } else {
                FerryColor.surfaceRaised()
            },
        ),
    )
}
