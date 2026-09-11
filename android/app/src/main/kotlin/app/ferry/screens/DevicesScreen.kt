package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.DeviceRow
import app.ferry.components.EmptyState
import app.ferry.components.ErrorBlock
import app.ferry.components.PresenceControl
import app.ferry.components.TransferRow
import app.ferry.components.ferryIconFor
import app.ferry.model.DeviceInfo
import app.ferry.model.ThreePartError
import app.ferry.model.TransferGroup

// Devices, the home screen. docs-v2/ia.md, On the phone.
//
// One row per paired device, its transfers under it, and Settings from the
// top bar. There is no loading state; the list is local and instant.
//
// There is no device detail screen. A phone holds one Mac, so a tap that
// selects it changes nothing: the transfers sit under the row here, and the
// four facts about the device fold into Settings. If a second Mac is ever
// paired the row becomes a destination and this reverses — and nothing else
// in the structure changes.
//
// The presence control is pinned under the top bar rather than filed in
// Settings. It is a mode with a consequence, reached in a hotel lobby in a
// hurry, and it is the one true fact Ferry can state about itself before
// anything is paired — so it shows in the empty state too.
//
// The error block above the list is what a missing all files access grant
// looks like. docs-v2/ia.md asks for exactly that: Devices still works, and
// the reason nothing can be served is on the screen with a way to fix it.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesScreen(
    devices: List<DeviceInfo>,
    // Every Transfers row, already grouped. The screen is handed one list
    // and does not know whether a row is a batch or a lone transfer.
    groupsFor: (DeviceInfo) -> List<TransferGroup>,
    isAdvertising: Boolean,
    wifiPresence: Boolean,
    error: ThreePartError?,
    errorActionLabel: String?,
    onErrorAction: (() -> Unit)?,
    onSetAdvertising: (Boolean) -> Unit,
    onPairClick: () -> Unit,
    onSettingsClick: () -> Unit,
    onRetryGroup: (TransferGroup) -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.devices_title)) },
                actions = {
                    IconButton(onClick = onSettingsClick) {
                        Icon(
                            imageVector = ferryIconFor(FerryIcon.settings),
                            contentDescription = stringResource(R.string.cd_settings_action),
                        )
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = FerryColor.surface(),
                ),
            )
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding)) {
            PresenceControl(
                isAdvertising = isAdvertising,
                wifiPresence = wifiPresence,
                onChange = onSetAdvertising,
            )
            HorizontalDivider(color = FerryColor.border())

            if (error != null) {
                ErrorBlock(
                    error = error,
                    onAction = onErrorAction,
                    actionLabel = errorActionLabel,
                    modifier = Modifier.padding(FerrySpace.s4),
                )
            }

            if (devices.isEmpty()) {
                // The Pair control lives here, in the empty state, and
                // nowhere else on this screen. Two controls that do one
                // thing is one too many.
                EmptyState(
                    line = stringResource(R.string.devices_empty_line),
                    actionLabel = stringResource(R.string.action_pair),
                    onAction = onPairClick,
                    modifier = Modifier.weight(1f),
                )
            } else {
                LazyColumn(modifier = Modifier.weight(1f)) {
                    items(devices, key = { it.id }) { device ->
                        DeviceRow(device = device)

                        val groups = groupsFor(device)
                        Text(
                            text = stringResource(R.string.device_section_transfers),
                            style = FerryFont.label(),
                            color = FerryColor.textSecondary(),
                            modifier = Modifier.padding(
                                start = FerrySpace.s4,
                                end = FerrySpace.s4,
                                top = FerrySpace.s3,
                                bottom = FerrySpace.s1,
                            ),
                        )
                        if (groups.isEmpty()) {
                            Text(
                                text = stringResource(R.string.transfers_empty_line),
                                style = FerryFont.body(),
                                color = FerryColor.textSecondary(),
                                modifier = Modifier.padding(horizontal = FerrySpace.s4),
                            )
                        } else {
                            groups.forEach { group ->
                                TransferRow(
                                    group = group,
                                    onRetry = { onRetryGroup(group) },
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .padding(horizontal = FerrySpace.s4),
                                )
                            }
                        }
                        HorizontalDivider(color = FerryColor.border())
                    }
                }
            }
        }
    }
}
