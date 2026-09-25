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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.formatDate
import app.ferry.model.DeviceInfo
import app.ferry.model.isCurrentNetworkTrusted

// Settings. docs/ia.md, Settings, the phone.
//
// Reachability has left this screen. It was a switch filed under
// preferences; it is a mode with a consequence, and it now lives where
// presence lives — pinned on Devices and in the notification.
//
// What is here instead: the two facts about this phone, the three
// permissions Android owns, the Networks section, the access log, and the
// paired device with Forget. Forget is here rather than on Devices because
// a phone has one Mac and no device detail screen, so this is that
// screen's four facts, folded in.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    deviceName: String,
    sharedRootName: String,
    devices: List<DeviceInfo>,
    allFilesAccessGranted: Boolean,
    notificationsAllowed: Boolean,
    locationGranted: Boolean,
    // The Networks section, docs/engine-contract.md item 18. Null when the
    // network cannot be read: Wi-Fi off, location refused, or unknown.
    currentNetworkName: String?,
    trustedNetworks: List<String>,
    isAdvertising: Boolean,
    wifiPresence: Boolean,
    onOpenAllFilesAccess: () -> Unit,
    onOpenNotificationSettings: () -> Unit,
    onOpenAppSettings: () -> Unit,
    onOpenAccessLog: () -> Unit,
    onTrustCurrentNetwork: () -> Unit,
    onForgetNetwork: (String) -> Unit,
    onForget: (DeviceInfo) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // docs/audits/oss-looks.md M2. Forgetting a device used to run at once,
    // from one tap on a plain text button. This holds the device a Forget
    // tap named, until the confirm dialog below is answered.
    var pendingForget by remember { mutableStateOf<DeviceInfo?>(null) }

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
            // docs/audits/oss-looks.md M5. Neither app said what the cable
            // path needs before it fails. This is the phone's own half of
            // that requirement, next to the other facts about this phone.
            Text(
                text = stringResource(R.string.settings_usb_requirement),
                style = FerryFont.caption(),
                color = FerryColor.textSecondary(),
                modifier = Modifier.padding(
                    start = FerrySpace.s4,
                    end = FerrySpace.s4,
                    bottom = FerrySpace.s2,
                ),
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
            Permission(
                label = stringResource(R.string.settings_location_label),
                granted = locationGranted,
                grantedWord = stringResource(R.string.settings_status_granted),
                notGrantedWord = stringResource(R.string.settings_status_not_granted),
                onClick = onOpenAppSettings,
            )

            NetworksSection(
                currentNetworkName = currentNetworkName,
                trustedNetworks = trustedNetworks,
                isAdvertising = isAdvertising,
                wifiPresence = wifiPresence,
                onTrustCurrentNetwork = onTrustCurrentNetwork,
                onForgetNetwork = onForgetNetwork,
                onOpenAppSettings = onOpenAppSettings,
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
                    // docs/audits/oss-looks.md M2. One tap used to forget
                    // the device at once. This tap now only names it; the
                    // dialog below is what actually forgets it.
                    TextButton(
                        onClick = { pendingForget = device },
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

    // docs/audits/oss-looks.md M2. Getting a forgotten device back needs
    // both devices and a new pairing, so the dialog states that, and its
    // destructive button is the one in Material's own error colour.
    pendingForget?.let { target ->
        AlertDialog(
            onDismissRequest = { pendingForget = null },
            title = { Text(stringResource(R.string.forget_device_title, target.name)) },
            text = { Text(stringResource(R.string.forget_device_body, target.name)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingForget = null
                        onForget(target)
                    },
                ) {
                    Text(
                        text = stringResource(R.string.action_forget_this_device),
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingForget = null }) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
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

// The Networks section. docs/engine-contract.md item 18, docs/ia.md
// Settings, the phone.
//
// The current network row offers Trust only while it is known and not
// already trusted: an unknown network cannot be trusted by name, and a
// trusted one has nothing left to offer here. The explanatory line at the
// bottom appears only while advertising is on and Wi-Fi presence is off,
// because that is the one state a person cannot otherwise explain to
// themselves — presence being off while not advertising needs no further
// word, that is what the switch above already says.
@Composable
private fun NetworksSection(
    currentNetworkName: String?,
    trustedNetworks: List<String>,
    isAdvertising: Boolean,
    wifiPresence: Boolean,
    onTrustCurrentNetwork: () -> Unit,
    onForgetNetwork: (String) -> Unit,
    onOpenAppSettings: () -> Unit,
) {
    GroupHeader(stringResource(R.string.settings_group_networks))

    if (currentNetworkName != null) {
        val trusted = isCurrentNetworkTrusted(currentNetworkName, trustedNetworks)
        ListItem(
            headlineContent = {
                Text(
                    text = stringResource(R.string.settings_network_current_label),
                    style = FerryFont.body(),
                )
            },
            supportingContent = {
                Text(
                    text = currentNetworkName,
                    style = FerryFont.caption(),
                    color = FerryColor.textSecondary(),
                )
            },
            trailingContent = if (trusted) {
                null
            } else {
                {
                    TextButton(onClick = onTrustCurrentNetwork) {
                        Text(stringResource(R.string.settings_network_trust))
                    }
                }
            },
            colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
        )
    }

    trustedNetworks.forEach { name ->
        ListItem(
            headlineContent = { Text(text = name, style = FerryFont.body()) },
            trailingContent = {
                TextButton(onClick = { onForgetNetwork(name) }) {
                    Text(
                        text = stringResource(R.string.action_remove),
                        color = FerryColor.text(),
                    )
                }
            },
            colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
        )
    }

    if (isAdvertising && !wifiPresence) {
        if (currentNetworkName == null) {
            ListItem(
                headlineContent = {
                    Text(
                        text = stringResource(R.string.settings_network_unknown_name),
                        style = FerryFont.body(),
                    )
                },
                trailingContent = {
                    IconButton(onClick = onOpenAppSettings) {
                        Icon(
                            imageVector = Icons.Outlined.OpenInNew,
                            contentDescription = stringResource(R.string.action_open_settings),
                            tint = FerryColor.textSecondary(),
                        )
                    }
                },
                colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
            )
        } else {
            ListItem(
                headlineContent = {
                    Text(
                        text = stringResource(R.string.presence_quiet_on_network),
                        style = FerryFont.body(),
                    )
                },
                colors = ListItemDefaults.colors(containerColor = FerryColor.surface()),
            )
        }
    }
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
