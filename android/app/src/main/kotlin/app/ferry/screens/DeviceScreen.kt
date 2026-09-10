package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.EmptyState
import app.ferry.components.ProgressLine
import app.ferry.components.TransportBadge
import app.ferry.formatDate
import app.ferry.model.ConnectionState
import app.ferry.model.DeviceInfo
import app.ferry.model.TransferInfo
import app.ferry.model.TransferState

// A paired Mac's detail: Transfers, newest first, then Info. docs/ia.md.
// Info carries the name, the transport, the key fingerprint, and Forget.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceScreen(
    device: DeviceInfo,
    transfers: List<TransferInfo>,
    onBack: () -> Unit,
    onForget: () -> Unit,
    onRetryTransfer: (TransferInfo) -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
        topBar = {
            TopAppBar(
                title = { Text(device.name) },
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
        LazyColumn(modifier = Modifier.padding(padding)) {
            item {
                SectionHeader(stringResource(R.string.device_section_transfers))
            }
            if (transfers.isEmpty()) {
                item {
                    EmptyState(line = stringResource(R.string.transfers_empty_line))
                }
            } else {
                items(transfers, key = { it.id }) { transfer ->
                    TransferRow(
                        transfer = transfer,
                        deviceState = device.connectionState,
                        onRetry = { onRetryTransfer(transfer) },
                    )
                }
            }

            item {
                HorizontalDivider(color = FerryColor.border())
                SectionHeader(stringResource(R.string.device_section_info))
            }
            item {
                Column(
                    modifier = Modifier.padding(
                        horizontal = FerrySpace.s4,
                        vertical = FerrySpace.s2,
                    ),
                ) {
                    Text(text = device.name, style = FerryFont.body(), color = FerryColor.text())
                    Spacer(Modifier.height(FerrySpace.s2))
                    TransportBadge(
                        transport = device.transport,
                        state = device.connectionState,
                    )
                    Spacer(Modifier.height(FerrySpace.s2))
                    Text(
                        text = stringResource(
                            R.string.device_info_paired_label,
                            formatDate(device.pairedUnixSecs),
                        ),
                        style = FerryFont.body(),
                        color = FerryColor.textSecondary(),
                    )
                    Spacer(Modifier.height(FerrySpace.s2))
                    Text(
                        text = stringResource(R.string.device_info_fingerprint_label),
                        style = FerryFont.label(),
                        color = FerryColor.textSecondary(),
                    )
                    Text(
                        text = device.keyFingerprint,
                        style = FerryFont.mono(),
                        color = FerryColor.text(),
                    )
                    Spacer(Modifier.height(FerrySpace.s4))
                    OutlinedButton(onClick = onForget) {
                        Text(stringResource(R.string.action_forget_this_mac))
                    }
                }
            }
        }
    }
}

@Composable
private fun SectionHeader(title: String) {
    Text(
        text = title,
        style = FerryFont.title(),
        color = FerryColor.text(),
        modifier = Modifier.padding(FerrySpace.s4),
    )
}

// The badge on an active transfer shows the transport carrying it and the
// device's own speed. The engine reports speed per device, not per
// transfer, so this is the speed there is.
@Composable
private fun TransferRow(
    transfer: TransferInfo,
    deviceState: ConnectionState,
    onRetry: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = FerrySpace.s4, vertical = FerrySpace.s2),
    ) {
        Row(modifier = Modifier.fillMaxWidth()) {
            Text(
                text = transfer.fileName,
                style = FerryFont.body(),
                color = FerryColor.text(),
                modifier = Modifier.weight(1f),
            )
            // The badge appears on every active transfer, docs/components.md.
            if (transfer.state == TransferState.Active) {
                TransportBadge(transport = transfer.transport, state = deviceState)
            }
        }
        Spacer(Modifier.height(FerrySpace.s1))
        ProgressLine(transfer = transfer, onRetry = onRetry)
    }
}
