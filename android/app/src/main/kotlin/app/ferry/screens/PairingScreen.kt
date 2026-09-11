package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
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
import app.ferry.engine.FerryEngine
import app.ferry.threePartError
import kotlinx.coroutines.delay
import uniffi.ferry_runtime.PairingState

// Pairing, a full screen flow on the phone. docs/ia.md's phone first run,
// steps 5 to 7.
//
// The engine drives every state. This screen turns pairing on when it
// opens, draws whatever state the engine reports, and forwards the two
// taps a person can make.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingScreen(
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by FerryEngine.pairing.collectAsState()
    val shortCode by FerryEngine.shortCode.collectAsState()
    val reachable by FerryEngine.reachable.collectAsState()

    // Pairing needs the phone to be reachable, because the Mac dials it.
    // Devices turns the switch on as part of the Pair tap, and the service
    // that does it starts on its own thread, so this waits for the engine
    // to report reachable before it asks for pairing.
    LaunchedEffect(reachable) {
        if (reachable) {
            FerryEngine.startPairing()
        }
    }

    val cancel = {
        FerryEngine.cancelPairing()
        onDone()
    }

    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.pairing_title)) },
                navigationIcon = {
                    IconButton(onClick = cancel) {
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
            when (val current = state) {
                // Idle is the state before the engine has answered
                // startPairing, and Found belongs to the Mac, which browses
                // and picks. Both draw the waiting screen, because that is
                // what the phone is doing in either case.
                is PairingState.Idle -> WaitingContent(shortCode = shortCode, onCancel = cancel)
                is PairingState.Waiting -> WaitingContent(shortCode = shortCode, onCancel = cancel)
                is PairingState.Found -> WaitingContent(shortCode = shortCode, onCancel = cancel)

                // Offering and Requested are the Mac's half of the scan
                // method (item 12); this phone never starts pairing by
                // scan today, so it never sees either, but the sealed
                // class still needs a branch for both. The phone's own
                // scan screen is a later, Kotlin-side design pass; until
                // then this is the same honest "waiting" screen as Idle.
                is PairingState.Offering -> WaitingContent(shortCode = shortCode, onCancel = cancel)
                is PairingState.Requested -> WaitingContent(shortCode = shortCode, onCancel = cancel)

                is PairingState.Code -> PairingCode(
                    code = groupOfThree(current.code),
                    onConfirm = { FerryEngine.confirmPairing(true) },
                    onCancel = {
                        // Rejecting drops the device the code belongs to.
                        // Leaving pairing after that ends the whole flow,
                        // which is what the Cancel control says it does.
                        FerryEngine.confirmPairing(false)
                        cancel()
                    },
                )

                is PairingState.Confirmed -> {
                    // docs/ia.md: confirmed returns to Devices, where the
                    // new Mac is now listed.
                    ConfirmedContent()
                    LaunchedEffect(current.device.keyHex) {
                        // docs/components.md holds the paired icon for one
                        // second before the view closes.
                        delay(CONFIRMED_MILLIS)
                        FerryEngine.cancelPairing()
                        onDone()
                    }
                }

                is PairingState.Failed -> ErrorBlock(
                    error = threePartError(current.error),
                    onAction = { FerryEngine.startPairing() },
                )
            }
        }
    }
}

// How long the paired icon stays before Devices comes back.
private const val CONFIRMED_MILLIS = 1_000L

// The engine reports six digits with nothing between them. PairingCode
// draws two groups of three, so the string is split here.
private fun groupOfThree(code: String): String =
    if (code.length == 6) code.substring(0, 3) + " " + code.substring(3) else code

@Composable
private fun WaitingContent(shortCode: String?, onCancel: () -> Unit) {
    Column {
        Text(
            text = stringResource(R.string.pairing_waiting_title),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
        Spacer(Modifier.height(FerrySpace.s4))
        if (shortCode != null) {
            Text(
                text = stringResource(R.string.pairing_short_code_label),
                style = FerryFont.label(),
                color = FerryColor.textSecondary(),
            )
            Text(
                text = shortCode,
                style = FerryFont.display(),
                color = FerryColor.text(),
            )
        }
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
