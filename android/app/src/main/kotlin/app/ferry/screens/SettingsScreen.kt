package app.ferry.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.OpenInNew
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.formatDate
import app.ferry.model.DeviceInfo

// Settings. docs-v2/ia.md, Settings, the phone.
//
// Reachability has left this screen. It was a switch filed under
// preferences; it is a mode with a consequence, and it now lives where
// presence lives — pinned on Devices and in the notification.
//
// What is here instead: the two facts about this phone, the two permissions
// Android owns, the access log, and the paired device with Forget. Forget
// is here rather than on Devices because a phone has one Mac and no device
// detail screen, so this is that screen's four facts, folded in.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    deviceName: String,
    sharedRootName: String,
    devices: List<DeviceInfo>,
    allFilesAccessGranted: Boolean,
    notificationsAllowed: Boolean,
    onOpenAllFilesAccess: () -> Unit,
    onOpenNotificationSettings: () -> Unit,
    onOpenAccessLog: () -> Unit,
    onForget: (DeviceInfo) -> Unit,
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
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = FerryColor.surface(),
                ),
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
        ) {
            GroupHeader(stringResource(R.string.settings_group_this_phone))
            // The name sent in hello. The engine takes it from the model,
            // and it is not editable in phase 1, so it is stated and not
            // offered as a field.
            Fact(
                label = stringResource(R.string.settings_name_label),
                value = deviceName,
            )
            Fact(
                label = stringResource(R.string.settings_shared_storage_label),
                value = sharedRootName,
                caption = stringResource(R.string.settings_shared_storage_caption),
            )

            GroupHeader(stringResource(R.string.settings_group_permissions))
            Permission(
                label = stringResource(R.string.settings_all_files_access_label),
                granted = allFilesAccessGranted,
                grantedWord = stringResource(R.string.settings_status_granted),
                notGrantedWord = stringResource(R.string.settings_status_not_granted),
                onClick = onOpenAllFilesAccess,
            )
            Permission(
                label = stringResource(R.string.settings_notifications_label),
                granted = notificationsAllowed,
                grantedWord = stringResource(R.string.settings_status_allowed),
                notGrantedWord = stringResource(R.string.settings_status_not_allowed),
                onClick = onOpenNotificationSettings,
            )

            GroupHeader(stringResource(R.string.settings_group_record))
            ListItem(
                modifier = Modifier.clickable(onClick = onOpenAccessLog),
                headlineContent = {
                    Text(
                        text = stringResource(R.string.access_log_title),
                        style = FerryFont.body(),
                    )
                },
                trailingContent = {
                    Icon(
                        imageVector = Icons.AutoMirrored.Outlined.KeyboardArrowRight,
                        contentDescription = null,
                        tint = FerryColor.textSecondary(),
                    )
                },
                colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
            )

            if (devices.isNotEmpty()) {
                GroupHeader(stringResource(R.string.settings_group_paired))
                devices.forEach { device ->
                    Fact(
                        label = device.name,
                        value = device.keyFingerprint,
                        caption = stringResource(
                            R.string.device_info_paired_label,
                            formatDate(device.pairedUnixSecs),
                        ),
                        valueIsMono = true,
                    )
                    // Destructive, and the platform's own colour for it.
                    // No danger token: Material has already answered this.
                    TextButton(
                        onClick = { onForget(device) },
                        modifier = Modifier.padding(horizontal = FerrySpace.s3),
                    ) {
                        Text(
                            text = stringResource(R.string.action_forget_this_device),
                            color = FerryColor.text(),
                        )
                    }
                    HorizontalDivider(color = FerryColor.border())
                }
            }
        }
    }
}

@Composable
private fun GroupHeader(title: String) {
    Text(
        text = title,
        style = FerryFont.label(),
        color = FerryColor.accentText(),
        modifier = Modifier.padding(
            start = FerrySpace.s4,
            end = FerrySpace.s4,
            top = FerrySpace.s4,
            bottom = FerrySpace.s1,
        ),
    )
}

// One stated fact. Not a control: nothing here is editable in phase 1, and
// a row that looks tappable and is not is a small lie.
@Composable
private fun Fact(
    label: String,
    value: String,
    caption: String? = null,
    valueIsMono: Boolean = false,
) {
    ListItem(
        headlineContent = { Text(text = label, style = FerryFont.body()) },
        supportingContent = {
            Column {
                Text(
                    text = value,
                    // Mono marks a machine-produced value, which a key
                    // fingerprint is and a device name is not.
                    style = if (valueIsMono) FerryFont.mono() else FerryFont.caption(),
                    color = FerryColor.textSecondary(),
                )
                if (caption != null) {
                    Text(
                        text = caption,
                        style = FerryFont.caption(),
                        color = FerryColor.textSecondary(),
                    )
                }
            }
        },
        colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
    )
}

// One permission Android owns, with the word for its state and a way to the
// system screen that changes it.
@Composable
private fun Permission(
    label: String,
    granted: Boolean,
    grantedWord: String,
    notGrantedWord: String,
    onClick: () -> Unit,
) {
    ListItem(
        modifier = Modifier.clickable(onClick = onClick),
        headlineContent = { Text(text = label, style = FerryFont.body()) },
        supportingContent = {
            Text(
                text = if (granted) grantedWord else notGrantedWord,
                style = FerryFont.caption(),
                color = FerryColor.textSecondary(),
            )
        },
        trailingContent = {
            Icon(
                imageVector = Icons.Outlined.OpenInNew,
                contentDescription = null,
                tint = FerryColor.textSecondary(),
            )
        },
        colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
    )
}
