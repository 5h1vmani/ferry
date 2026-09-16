package app.ferry

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import app.ferry.engine.FerryEngine
import app.ferry.model.ThreePartError
import app.ferry.model.accessDaysOf
import app.ferry.model.toUi
import app.ferry.model.transferGroupsFor
import app.ferry.screens.AccessLogScreen
import app.ferry.screens.DevicesScreen
import app.ferry.screens.FirstRunScreen
import app.ferry.screens.PairingScreen
import app.ferry.screens.SettingsScreen

// The screens of the phone app. No navigation library: this sealed class is
// the whole of Ferry's navigation state, and FerryApp's `when` is the whole
// of its navigation logic.
//
// There is no Device screen. A phone holds one Mac, so selecting it changes
// nothing: its transfers are on Devices and its four facts are in Settings.
// docs-v2/ia.md, On the phone.
sealed class Screen {
    data object Devices : Screen()
    data object Pairing : Screen()
    data object Settings : Screen()
    data object AccessLog : Screen()
}

// Screen holds no Parcelable state of its own, so rememberSaveable in
// FerryApp stores this name instead, across rotation and process death.
private const val SCREEN_DEVICES = "Devices"
private const val SCREEN_PAIRING = "Pairing"
private const val SCREEN_SETTINGS = "Settings"
private const val SCREEN_ACCESS_LOG = "AccessLog"

private fun Screen.toSavedName(): String = when (this) {
    Screen.Devices -> SCREEN_DEVICES
    Screen.Pairing -> SCREEN_PAIRING
    Screen.Settings -> SCREEN_SETTINGS
    Screen.AccessLog -> SCREEN_ACCESS_LOG
}

private fun screenFromSavedName(name: String): Screen = when (name) {
    SCREEN_PAIRING -> Screen.Pairing
    SCREEN_SETTINGS -> Screen.Settings
    SCREEN_ACCESS_LOG -> Screen.AccessLog
    else -> Screen.Devices
}

// The one activity's content. Every value on screen comes from the engine's
// flows, mapped by model/Mapping.kt. Nothing here is sample data, and
// nothing here formats a number or composes a sentence.
@Composable
fun FerryApp(
    onGrantFirstRunAccess: () -> Unit,
    onSkipFirstRun: () -> Unit,
    onOpenAllFilesAccess: () -> Unit,
    onOpenNotificationSettings: () -> Unit,
    onOpenAppSettings: () -> Unit,
    onRequestCamera: () -> Unit,
    onRequestLocation: () -> Unit,
    onSetAdvertising: (Boolean) -> Unit,
    onSendFilesClick: () -> Unit,
) {
    val darkTheme = isSystemInDarkTheme()
    val baseScheme = if (darkTheme) darkColorScheme() else lightColorScheme()
    val colorScheme = baseScheme.copy(
        primary = FerryColor.accent(),
        background = FerryColor.background(),
        surface = FerryColor.surface(),
        onSurface = FerryColor.text(),
    )

    // remember alone loses this to a rotation: the activity is destroyed
    // and rebuilt, and Devices was the only screen that ever came back.
    // Screen has no Parcelable of its own, so its name is what is saved.
    var screenName by rememberSaveable { mutableStateOf(Screen.Devices.toSavedName()) }
    val screen: Screen = screenFromSavedName(screenName)
    fun goTo(next: Screen) {
        screenName = next.toSavedName()
    }

    // A share from another app shows Devices, whatever screen was in front:
    // the empty state with no Mac paired, or the pushed transfer once one
    // is. docs/ux-fix-plan.md item 1. The counter is 0 before any share, so
    // the first composition does not navigate anywhere on its own.
    val shareNavigateHome by ShareIntake.navigateHome.collectAsState()
    LaunchedEffect(shareNavigateHome) {
        if (shareNavigateHome > 0) {
            goTo(Screen.Devices)
        }
    }

    // The system back gesture otherwise finishes the activity from every
    // screen, whatever screen is showing. Each branch does what that
    // screen's own back control does.
    BackHandler(enabled = screen != Screen.Devices) {
        when (screen) {
            is Screen.Pairing -> {
                FerryEngine.cancelPairing()
                goTo(Screen.Devices)
            }
            is Screen.Settings -> goTo(Screen.Devices)
            is Screen.AccessLog -> goTo(Screen.Settings)
            is Screen.Devices -> Unit
        }
    }

    val allFilesAccess by Permissions.allFilesAccess.collectAsState()
    val notificationsAllowed by Permissions.notifications.collectAsState()
    val cameraGranted by Permissions.camera.collectAsState()
    val cameraRefused by Permissions.cameraRefused.collectAsState()
    val locationGranted by Permissions.location.collectAsState()
    val firstRunDone by Permissions.firstRunDone.collectAsState()

    val engineDevices by FerryEngine.devices.collectAsState()
    val engineTransfers by FerryEngine.transfers.collectAsState()
    val engineBatches by FerryEngine.batches.collectAsState()
    val engineAccessLog by FerryEngine.accessLog.collectAsState()
    val engineError by FerryEngine.error.collectAsState()
    val isAdvertising by FerryEngine.reachable.collectAsState()
    val currentNetwork by FerryEngine.network.collectAsState()
    val wifiPresence by FerryEngine.wifiPresence.collectAsState()
    val trustedNetworks by FerryEngine.trustedNetworks.collectAsState()

    val devices = engineDevices.map { it.toUi() }
    val accessDays = accessDaysOf(engineAccessLog.map { it.toUi() })

    // A file a share could not read, or null: docs/ux-fix-plan.md item 1.
    // This is an app-side fault, not an engine code, so its three parts are
    // built from strings.xml here rather than through threePartError.
    val shareUnreadableName by ShareIntake.unreadableName.collectAsState()

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
    } else if (shareUnreadableName != null) {
        errorWords = ThreePartError(
            stopped = stringResource(R.string.share_unreadable_stopped),
            why = stringResource(R.string.share_unreadable_why, shareUnreadableName!!),
            todo = stringResource(R.string.share_unreadable_todo),
        )
    } else {
        val failure = engineError
        if (failure != null) {
            errorWords = threePartError(failure)
        }
    }

    MaterialTheme(colorScheme = colorScheme) {
        if (!firstRunDone) {
            FirstRunScreen(
                onGrantAccess = onGrantFirstRunAccess,
                onSkip = onSkipFirstRun,
            )
            return@MaterialTheme
        }

        when (screen) {
            is Screen.Devices -> DevicesScreen(
                devices = devices,
                // The one place transfers are grouped. A screen is handed
                // one list of rows and never asks whether a row is a batch.
                groupsFor = { device ->
                    transferGroupsFor(
                        deviceId = device.id,
                        batches = engineBatches,
                        transfers = engineTransfers,
                    )
                },
                isAdvertising = isAdvertising,
                wifiPresence = wifiPresence,
                error = errorWords,
                errorActionLabel = errorActionLabel,
                onErrorAction = errorAction,
                onSetAdvertising = onSetAdvertising,
                onPairClick = {
                    // Pairing by code needs the phone to be reachable,
                    // because the Mac dials it. Turning the switch on is
                    // part of the tap, not something to ask for first. A
                    // scan does not need it, and turning it on anyway
                    // costs nothing and keeps one tap doing one thing.
                    if (!isAdvertising) {
                        onSetAdvertising(true)
                    }
                    goTo(Screen.Pairing)
                },
                onSettingsClick = { goTo(Screen.Settings) },
                onSendFilesClick = onSendFilesClick,
                onRetryGroup = { group ->
                    // A batch retries every failed transfer in it, in one
                    // tap rather than one tap per file. A lone transfer's
                    // id is its own.
                    if (group.isSingleFile) {
                        FerryEngine.retry(group.id)
                    } else {
                        FerryEngine.retryBatch(group.id)
                    }
                },
            )

            is Screen.Pairing -> PairingScreen(
                cameraGranted = cameraGranted,
                cameraRefused = cameraRefused,
                onRequestCamera = onRequestCamera,
                onOpenAppSettings = onOpenAppSettings,
                // Only the network name needs location, and only pairing
                // needs the name, so the Choosing screen is the first
                // moment worth asking, with the line explaining why
                // rendered before the prompt fires. onRequestLocation is
                // a no-op past the first time.
                onRequestLocation = onRequestLocation,
                onDone = { goTo(Screen.Devices) },
            )

            is Screen.Settings -> SettingsScreen(
                deviceName = android.os.Build.MODEL,
                sharedRootName = stringResource(R.string.settings_shared_storage_value),
                devices = devices,
                allFilesAccessGranted = allFilesAccess,
                notificationsAllowed = notificationsAllowed,
                locationGranted = locationGranted,
                currentNetworkName = currentNetwork,
                trustedNetworks = trustedNetworks,
                isAdvertising = isAdvertising,
                wifiPresence = wifiPresence,
                onOpenAllFilesAccess = onOpenAllFilesAccess,
                onOpenNotificationSettings = onOpenNotificationSettings,
                onOpenAppSettings = onOpenAppSettings,
                onOpenAccessLog = { goTo(Screen.AccessLog) },
                onTrustCurrentNetwork = {
                    currentNetwork?.let { FerryEngine.trustNetwork(it) }
                },
                onForgetNetwork = { name -> FerryEngine.forgetNetwork(name) },
                onForget = { device ->
                    FerryEngine.forget(device.id)
                    goTo(Screen.Devices)
                },
                onBack = { goTo(Screen.Devices) },
            )

            is Screen.AccessLog -> AccessLogScreen(
                days = accessDays,
                // A log entry holds a key; only the device list holds a
                // name. A forgotten device's entries outlive it, so an
                // unknown key falls back to its fingerprint rather than to
                // a guess or an empty subject.
                peerNameFor = { keyHex ->
                    devices.firstOrNull { it.id == keyHex }?.name
                        ?: app.ferry.model.fingerprintOf(keyHex)
                },
                onBack = { goTo(Screen.Settings) },
            )
        }
    }
}
