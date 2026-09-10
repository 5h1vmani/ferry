package app.ferry.screens

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringArrayResource
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R

// Settings, reached from the Devices top bar. docs/ia.md: Reachable, the
// fixed shared folder list, all files access, and notifications on API 33+.
// Forgetting a device lives on that device's own screen, not here.
//
// All files access and notifications reflect real system state once the
// core is linked. Here they are fixed sample values, like every other
// screen; only Reachable has a switch worth flipping by hand.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    var reachable by remember { mutableStateOf(true) }
    val allFilesAccessGranted = true
    val notificationsAllowed = true
    val sharedFolders = stringArrayResource(R.array.settings_shared_folder_names)

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
                    onCheckedChange = { reachable = it },
                    colors = SwitchDefaults.colors(checkedTrackColor = FerryColor.accent()),
                )
            }
            HorizontalDivider(color = FerryColor.border())

            // Shared folders, fixed, not editable in phase 1.
            Column(modifier = Modifier.padding(FerrySpace.s4)) {
                Text(
                    text = stringResource(R.string.settings_shared_folders_label),
                    style = FerryFont.body(),
                    color = FerryColor.text(),
                )
                Text(
                    text = sharedFolders.joinToString(", "),
                    style = FerryFont.body(),
                    color = FerryColor.textSecondary(),
                )
                Text(
                    text = stringResource(R.string.settings_shared_folders_caption),
                    style = FerryFont.caption(),
                    color = FerryColor.textSecondary(),
                )
            }
            HorizontalDivider(color = FerryColor.border())

            // All files access. Ferry cannot serve files without it.
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(FerrySpace.s4),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = stringResource(R.string.settings_all_files_access_label),
                        style = FerryFont.body(),
                        color = FerryColor.text(),
                    )
                    Text(
                        text = if (allFilesAccessGranted) {
                            stringResource(R.string.settings_status_granted)
                        } else {
                            stringResource(R.string.settings_status_not_granted)
                        },
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
                OutlinedButton(onClick = {
                    val intent = Intent(
                        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        Uri.parse("package:" + context.packageName),
                    )
                    context.startActivity(intent)
                }) {
                    Text(stringResource(R.string.action_open_settings))
                }
            }

            // Notifications, needed for the reachable notification. Android
            // 13 (API 33) is the first version that asks for it.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                HorizontalDivider(color = FerryColor.border())
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(FerrySpace.s4),
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = stringResource(R.string.settings_notifications_label),
                            style = FerryFont.body(),
                            color = FerryColor.text(),
                        )
                        Text(
                            text = if (notificationsAllowed) {
                                stringResource(R.string.settings_status_allowed)
                            } else {
                                stringResource(R.string.settings_status_not_allowed)
                            },
                            style = FerryFont.caption(),
                            color = FerryColor.textSecondary(),
                        )
                    }
                    OutlinedButton(onClick = {
                        val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                            .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                        context.startActivity(intent)
                    }) {
                        Text(stringResource(R.string.action_open_settings))
                    }
                }
            }
        }
    }
}
