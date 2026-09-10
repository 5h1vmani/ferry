package app.ferry.screens

import androidx.compose.foundation.layout.fillMaxSize
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
import app.ferry.R
import app.ferry.components.DeviceRow
import app.ferry.components.EmptyState
import app.ferry.components.ferryIconFor
import app.ferry.model.DeviceInfo

// Devices, the home screen. docs/ia.md: one row per paired Mac, a Pair
// control, and Settings from the top bar. There is no loading state; the
// list is local and instant.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesScreen(
    devices: List<DeviceInfo>,
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
        if (devices.isEmpty()) {
            EmptyState(
                line = stringResource(R.string.devices_empty_line),
                actionLabel = stringResource(R.string.action_pair),
                onAction = onPairClick,
                modifier = Modifier
                    .padding(padding)
                    .fillMaxSize(),
            )
        } else {
            LazyColumn(modifier = Modifier.padding(padding)) {
                items(devices, key = { it.id }) { device ->
                    DeviceRow(device = device, onClick = { onDeviceClick(device) })
                }
            }
        }
    }
}
