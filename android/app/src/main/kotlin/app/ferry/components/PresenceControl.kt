package app.ferry.components

import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.R

// Whether this phone advertises and accepts connections, and what that
// costs when it does not. docs/components.md, PresenceControl. Job 5's
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
    // True while this device advertises, browses, and accepts over Wi-Fi.
    // Meaningless while isAdvertising is false: reachable being off is
    // already the whole story then. docs/engine-contract.md item 18.
    wifiPresence: Boolean,
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
    // Advertising is on, but this network is not trusted, so nothing is
    // actually reachable on it. Shared with the reachable notification's
    // own text, so both surfaces say the same thing about the same state.
    val quiet = stringResource(R.string.presence_quiet_on_network)
    val description = when {
        !isAdvertising -> stringResource(R.string.cd_presence_off, consequence)
        !wifiPresence -> stringResource(R.string.cd_presence_quiet, quiet)
        else -> stringResource(R.string.cd_presence_on)
    }

    ListItem(
        // docs/audits/oss-looks.md M3. clearAndSetSemantics used to replace
        // the whole row's semantics, which dropped the Switch's own toggle
        // action along with it: TalkBack read the state but had no action
        // on it. toggleable below gives the row the real tap target, the
        // Switch role, and the checked state in one merged node; the
        // explicit contentDescription after it is what gets announced,
        // instead of the label and supporting text read out a second time.
        modifier = modifier
            .toggleable(
                value = isAdvertising,
                onValueChange = onChange,
                role = Role.Switch,
            )
            .semantics(mergeDescendants = true) { contentDescription = description },
        headlineContent = { Text(text = label, style = FerryFont.body()) },
        supportingContent = when {
            !isAdvertising -> {
                {
                    Text(
                        text = consequence,
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
            }
            !wifiPresence -> {
                {
                    Text(
                        text = quiet,
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
            }
            else -> null
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
                // The row above is the real tap target and carries the
                // Switch role and toggle action; a second, independent
                // click target here would be a duplicate for both a touch
                // and a screen reader user.
                onCheckedChange = null,
                // docs/audits/oss-looks.md M8. accent is step 9, which is
                // 2.59 to 1 against this row's accent_surface fill in dark
                // mode, below the 3 to 1 an icon or a track needs.
                // accent_text is step 11, already used for text against
                // this same background, and it clears 3 to 1 in both
                // themes: see the contrast script in the batch report.
                colors = SwitchDefaults.colors(checkedTrackColor = FerryColor.accentText()),
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
