package app.ferry

import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.model.SampleState
import app.ferry.screens.DeviceScreen
import app.ferry.screens.DevicesScreen
import app.ferry.screens.PairingScreen
import app.ferry.screens.SettingsScreen

// The four screens, docs/design.md's table. No navigation library: this
// sealed class is the whole of Ferry's navigation state, and FerryApp's
// `when` is the whole of its navigation logic.
sealed class Screen {
    data object Devices : Screen()
    data class Device(val deviceId: String) : Screen()
    data object Pairing : Screen()
    data object Settings : Screen()
}

// The one activity's content. Holds which screen is showing and today's
// stand-in for the Rust core: SampleState, toggled empty or populated by
// the debug switch at the top.
//
// TODO: the persistent "reachable" notification (docs/ia.md, "While the
// phone is reachable...") starts its foreground service from here, once
// Settings' Reachable switch is wired to something real. Nothing starts it
// yet; the manifest already declares the permission and service type it
// will need.
@Composable
fun FerryApp() {
    var screen by remember { mutableStateOf<Screen>(Screen.Devices) }
    var populated by remember { mutableStateOf(true) }

    val devices = if (populated) SampleState.devicesPopulated else SampleState.devicesEmpty
    val transfers = if (populated) SampleState.transfersPopulated else SampleState.transfersEmpty

    val darkTheme = isSystemInDarkTheme()
    val baseScheme = if (darkTheme) darkColorScheme() else lightColorScheme()
    val colorScheme = baseScheme.copy(
        primary = FerryColor.accent(),
        background = FerryColor.background(),
        surface = FerryColor.surface(),
        onSurface = FerryColor.text(),
    )

    MaterialTheme(colorScheme = colorScheme) {
        Column(modifier = Modifier.background(FerryColor.background())) {
            DebugSampleDataToggle(populated = populated, onToggle = { populated = it })

            when (val current = screen) {
                is Screen.Devices -> DevicesScreen(
                    devices = devices,
                    onDeviceClick = { device -> screen = Screen.Device(device.id) },
                    onPairClick = { screen = Screen.Pairing },
                    onSettingsClick = { screen = Screen.Settings },
                )

                is Screen.Device -> {
                    val device = devices.find { it.id == current.deviceId }
                    if (device != null) {
                        DeviceScreen(
                            device = device,
                            transfers = transfers.filter { it.deviceId == device.id },
                            onBack = { screen = Screen.Devices },
                            onForget = { screen = Screen.Devices },
                            // Sample state never changes at runtime, so a
                            // retry has nothing to do yet.
                            onRetryTransfer = {},
                        )
                    } else {
                        // The debug switch moved to "empty" while this
                        // device was open. Devices is always a safe screen
                        // to fall back to.
                        DevicesScreen(
                            devices = devices,
                            onDeviceClick = { d -> screen = Screen.Device(d.id) },
                            onPairClick = { screen = Screen.Pairing },
                            onSettingsClick = { screen = Screen.Settings },
                        )
                    }
                }

                is Screen.Pairing -> PairingScreen(
                    onCancel = { screen = Screen.Devices },
                    onConfirm = { screen = Screen.Devices },
                    // Sample state never changes at runtime, so a retry has
                    // nothing to do yet.
                    onRetry = {},
                    onBack = { screen = Screen.Devices },
                )

                is Screen.Settings -> SettingsScreen(onBack = { screen = Screen.Devices })
            }
        }
    }
}

// Debug only, not a docs/voice.md string: swaps SampleState between its
// empty and populated lists so every screen state can be seen without a
// real device. Not part of the product.
@Composable
private fun DebugSampleDataToggle(populated: Boolean, onToggle: (Boolean) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(FerryColor.surfaceRaised())
            .padding(FerrySpace.s2),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = stringResource(R.string.debug_sample_data_label),
            style = FerryFont.caption(),
            color = FerryColor.textSecondary(),
            modifier = Modifier.weight(1f),
        )
        Switch(checked = populated, onCheckedChange = onToggle)
    }
}
