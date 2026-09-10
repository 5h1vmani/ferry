package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.DeviceRow
import app.ferry.components.EmptyState
import app.ferry.components.ErrorBlock
import app.ferry.components.ferryIconFor
import app.ferry.model.DeviceInfo
import app.ferry.model.ThreePartError

// Devices, the home screen. docs/ia.md: one row per paired Mac, a Pair
// control, and Settings from the top bar. There is no loading state; the
// list is local and instant.
//
// The error block above the list is what a missing all files access grant
// looks like. docs/ia.md asks for exactly that: Devices still works, and
// the reason nothing can be served is on the screen with a way to fix it.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesScreen(
    devices: List<DeviceInfo>,
    error: ThreePartError?,
    errorActionLabel: String?,
    onErrorAction: (() -> Unit)?,
    onDeviceClick: (DeviceInfo) -> Unit,
    onPairClick: () -> Unit,
    onSettingsClick: () -> Unit,
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
            )
        },
        floatingActionButton = {
            if (devices.isNotEmpty()) {
                ExtendedFloatingActionButton(onClick = onPairClick) {
                    Icon(imageVector = ferryIconFor(FerryIcon.pair), contentDescription = null)
                    Text(stringResource(R.string.action_pair))
                }
            }
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding)) {
            if (error != null) {
                ErrorBlock(
                    error = error,
                    onAction = onErrorAction,
                    actionLabel = errorActionLabel,
                    modifier = Modifier.padding(FerrySpace.s4),
                )
            }
            if (devices.isEmpty()) {
                EmptyState(
                    line = stringResource(R.string.devices_empty_line),
                    actionLabel = stringResource(R.string.action_pair),
                    onAction = onPairClick,
                    modifier = Modifier.weight(1f),
                )
            } else {
                LazyColumn(modifier = Modifier.weight(1f)) {
                    items(devices, key = { it.id }) { device ->
                        DeviceRow(device = device, onClick = { onDeviceClick(device) })
                    }
                }
            }
        }
    }
}
