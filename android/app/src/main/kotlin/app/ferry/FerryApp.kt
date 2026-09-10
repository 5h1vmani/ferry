package app.ferry

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import app.ferry.engine.FerryEngine
import app.ferry.model.ThreePartError
import app.ferry.model.toUi
import app.ferry.screens.DeviceScreen
import app.ferry.screens.DevicesScreen
import app.ferry.screens.FirstRunScreen
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

// The one activity's content. Every value on screen comes from the engine's
// flows. Nothing here is sample data.
@Composable
fun FerryApp(
    onContinueFirstRun: () -> Unit,
    onOpenAllFilesAccess: () -> Unit,
    onOpenNotificationSettings: () -> Unit,
    onSetReachable: (Boolean) -> Unit,
) {
    val darkTheme = isSystemInDarkTheme()
    val baseScheme = if (darkTheme) darkColorScheme() else lightColorScheme()
    val colorScheme = baseScheme.copy(
        primary = FerryColor.accent(),
        background = FerryColor.background(),
        surface = FerryColor.surface(),
        onSurface = FerryColor.text(),
    )

    var screen by remember { mutableStateOf<Screen>(Screen.Devices) }

    val allFilesAccess by Permissions.allFilesAccess.collectAsState()
    val notificationsAllowed by Permissions.notifications.collectAsState()
    val firstRunDone by Permissions.firstRunDone.collectAsState()
    val engineDevices by FerryEngine.devices.collectAsState()
    val engineTransfers by FerryEngine.transfers.collectAsState()
    val engineError by FerryEngine.error.collectAsState()
    val reachable by FerryEngine.reachable.collectAsState()

    val devices = engineDevices.map { it.toUi() }
    val transfers = engineTransfers.map { it.toUi() }

    // Devices carries whatever is stopping Ferry from working, in the order
    // that matters. A missing all files access grant comes first, because
    // it is the one a person can fix, and it is why the engine did not
    // start at all.
    var errorWords: ThreePartError? = null
    var errorActionLabel: String? = null
    var errorAction: (() -> Unit)? = null
    if (!allFilesAccess) {
        errorWords = threePartError("Runtime::AllFilesAccess")
        errorActionLabel = stringResource(R.string.action_open_settings)
        errorAction = onOpenAllFilesAccess
    } else {
        val failure = engineError
        if (failure != null) {
            errorWords = threePartError(failure)
        }
    }

    MaterialTheme(colorScheme = colorScheme) {
        if (!firstRunDone) {
            // docs/ia.md, phone first run step 1. It is shown until the
            // person uses Continue. After that Devices carries the error
            // block, so the app opens rather than refusing to.
            FirstRunScreen(onContinue = onContinueFirstRun)
            return@MaterialTheme
        }

        when (val current = screen) {
            is Screen.Devices -> DevicesScreen(
                devices = devices,
                error = errorWords,
                errorActionLabel = errorActionLabel,
                onErrorAction = errorAction,
                onDeviceClick = { device -> screen = Screen.Device(device.id) },
                onPairClick = {
                    // Pairing needs the phone to be reachable, because the
                    // Mac dials it. Turning the switch on is part of the
                    // tap, not something to ask for first.
                    if (!reachable) {
                        onSetReachable(true)
                    }
                    screen = Screen.Pairing
                },
                onSettingsClick = { screen = Screen.Settings },
            )

            is Screen.Device -> {
                val device = devices.find { it.id == current.deviceId }
                if (device == null) {
                    // The Mac was forgotten, or the engine dropped it while
                    // this screen was open. Devices is always safe.
                    LaunchedEffect(current.deviceId) { screen = Screen.Devices }
                } else {
                    DeviceScreen(
                        device = device,
                        transfers = transfers.filter { it.deviceId == device.id },
                        onBack = { screen = Screen.Devices },
                        onForget = {
                            FerryEngine.forget(device.id)
                            screen = Screen.Devices
                        },
                        onRetryTransfer = { transfer -> FerryEngine.retry(transfer.id) },
                    )
                }
            }

            is Screen.Pairing -> PairingScreen(
                onDone = { screen = Screen.Devices },
            )

            is Screen.Settings -> SettingsScreen(
                reachable = reachable,
                allFilesAccessGranted = allFilesAccess,
                notificationsAllowed = notificationsAllowed,
                onSetReachable = onSetReachable,
                onOpenAllFilesAccess = onOpenAllFilesAccess,
                onOpenNotificationSettings = onOpenNotificationSettings,
                onBack = { screen = Screen.Devices },
            )
        }
    }
}
