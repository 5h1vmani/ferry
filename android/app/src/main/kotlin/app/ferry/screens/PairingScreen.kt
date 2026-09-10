package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.ErrorBlock
import app.ferry.components.PairingCode
import app.ferry.components.ferryIconFor
import app.ferry.model.PairingState
import app.ferry.model.SampleState

// Pairing, a full screen flow on the phone. docs/ia.md's phone first run,
// steps 5-7. A person moves through these states automatically once the
// core drives them; the segmented control here exists only so every state
// can be viewed in this static scaffold.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingScreen(
    onCancel: () -> Unit,
    onConfirm: () -> Unit,
    onRetry: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val states = listOf(
        SampleState.pairingWaiting,
        SampleState.pairingCode,
        SampleState.pairingConfirmed,
        SampleState.pairingFailed,
    )
    val labels = listOf(
        stringResource(R.string.pairing_segment_waiting),
        stringResource(R.string.pairing_segment_code),
        stringResource(R.string.pairing_segment_confirmed),
        stringResource(R.string.pairing_segment_failed),
    )
    var selected by remember { mutableIntStateOf(0) }

    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.pairing_title)) },
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
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(FerrySpace.s4)
                .fillMaxSize(),
        ) {
            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                labels.forEachIndexed { index, label ->
                    SegmentedButton(
                        selected = selected == index,
                        onClick = { selected = index },
                        shape = SegmentedButtonDefaults.itemShape(index = index, count = labels.size),
                    ) {
                        Text(label)
                    }
                }
            }
            Spacer(Modifier.height(FerrySpace.s6))
            when (val state = states[selected]) {
                is PairingState.Waiting -> WaitingContent(shortCode = state.shortCode, onCancel = onCancel)
                is PairingState.Code -> PairingCode(code = state.code, onConfirm = onConfirm, onCancel = onCancel)
                is PairingState.Confirmed -> ConfirmedContent()
                is PairingState.Failed -> ErrorBlock(error = state.error, onRetry = onRetry)
            }
        }
    }
}

@Composable
private fun WaitingContent(shortCode: String, onCancel: () -> Unit) {
    Column {
        Text(
            text = stringResource(R.string.pairing_waiting_title),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
        Spacer(Modifier.height(FerrySpace.s3))
        Text(
            text = stringResource(R.string.pairing_waiting_short_code_label, shortCode),
            style = FerryFont.mono(),
            color = FerryColor.textSecondary(),
        )
        Spacer(Modifier.height(FerrySpace.s5))
        OutlinedButton(onClick = onCancel) {
            Text(stringResource(R.string.action_cancel))
        }
    }
}

@Composable
private fun ConfirmedContent() {
    Column(horizontalAlignment = Alignment.Start) {
        Icon(
            imageVector = ferryIconFor(FerryIcon.paired),
            contentDescription = null,
            tint = FerryColor.accent(),
            modifier = Modifier.height(48.dp),
        )
        Spacer(Modifier.height(FerrySpace.s3))
        Text(
            text = stringResource(R.string.pairing_confirmed_label),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
    }
}
