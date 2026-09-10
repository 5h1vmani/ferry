package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.Permissions
import app.ferry.R

// Settings, reached from the Devices top bar. docs/ia.md: Reachable,
// shared storage, all files access, and notifications on Android 13 and
// later. Forgetting a device lives on that device's own screen, not here.
//
// Every value here is read from the engine or from Android. The switch
// starts and stops the reachable service, which is the thing that makes
// the phone reachable, so the switch and the notification cannot disagree.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    reachable: Boolean,
    allFilesAccessGranted: Boolean,
    notificationsAllowed: Boolean,
    onSetReachable: (Boolean) -> Unit,
    onOpenAllFilesAccess: () -> Unit,
    onOpenNotificationSettings: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.settings_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.cd_back),
                        )
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = FerryColor.surface()),
            )
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding)) {
            // Reachable.
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(FerrySpace.s4),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = stringResource(R.string.settings_reachable_label),
                        style = FerryFont.body(),
                        color = FerryColor.text(),
                    )
                    Text(
                        text = stringResource(R.string.settings_reachable_caption),
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
                Switch(
                    checked = reachable,
                    onCheckedChange = onSetReachable,
                    colors = SwitchDefaults.colors(checkedTrackColor = FerryColor.accent()),
                )
            }
            HorizontalDivider(color = FerryColor.border())

            // Shared storage. The engine serves one root, and on the phone
            // that root is internal storage.
            Column(modifier = Modifier.padding(FerrySpace.s4)) {
                Text(
                    text = stringResource(R.string.settings_shared_storage_label),
                    style = FerryFont.body(),
                    color = FerryColor.text(),
                )
                Text(
                    text = stringResource(R.string.settings_shared_storage_value),
                    style = FerryFont.body(),
                    color = FerryColor.textSecondary(),
                )
                Text(
                    text = stringResource(R.string.settings_shared_storage_caption),
                    style = FerryFont.caption(),
                    color = FerryColor.textSecondary(),
                )
            }
            HorizontalDivider(color = FerryColor.border())

            // All files access. Ferry cannot serve files without it.
            SettingWithSystemScreen(
                label = stringResource(R.string.settings_all_files_access_label),
                status = if (allFilesAccessGranted) {
                    stringResource(R.string.settings_status_granted)
                } else {
                    stringResource(R.string.settings_status_not_granted)
                },
                onOpen = onOpenAllFilesAccess,
            )

            // Notifications, needed for the reachable notification. Android
            // 13 is the first version that asks for it.
            if (Permissions.asksForNotifications()) {
                HorizontalDivider(color = FerryColor.border())
                SettingWithSystemScreen(
                    label = stringResource(R.string.settings_notifications_label),
                    status = if (notificationsAllowed) {
                        stringResource(R.string.settings_status_allowed)
                    } else {
                        stringResource(R.string.settings_status_not_allowed)
                    },
                    onOpen = onOpenNotificationSettings,
                )
            }
        }
    }
}

// One row that states what Android has granted and opens the system screen
// where it is changed.
@Composable
private fun SettingWithSystemScreen(
    label: String,
    status: String,
    onOpen: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(FerrySpace.s4),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(text = label, style = FerryFont.body(), color = FerryColor.text())
            Text(
                text = status,
                style = FerryFont.caption(),
                color = FerryColor.textSecondary(),
            )
        }
        OutlinedButton(onClick = onOpen) {
            Text(stringResource(R.string.action_open_settings))
        }
    }
}
