package app.ferry.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerryRadius
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.ErrorBlock
import app.ferry.components.PairingCode
import app.ferry.components.ferryIconFor
import app.ferry.engine.FerryEngine
import app.ferry.formatCountdown
import app.ferry.model.PairingMethod
import app.ferry.model.PairingStep
import app.ferry.model.pairingStepOf
import app.ferry.threePartError
import kotlinx.coroutines.delay

// Pairing, a full screen flow on the phone. docs-v2/ia.md, Pairing, the
// phone.
//
// Two ways in, and every step offers the other one. Scanning is fewer steps
// when both devices are in reach, which is when pairing happens, so it is
// first — but a phone with no camera, a refused permission, a Mac on a
// screen this phone cannot point at, and a person who would simply rather
// type all end up on the code path, and none of them should have to go back
// a screen to get there.
//
// Pairing is a once-ever job, so two ways in is one screen more, not two
// things to maintain forever. Both end at Confirmed, with the same device
// in the same list, and every screen after pairing is untouched.
//
// The engine drives every state. This screen chooses a method, draws
// whatever the engine reports, and forwards the taps a person can make.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingScreen(
    // True when the camera permission is granted. The screen asks for it
    // when a person taps to scan, not at first run.
    cameraGranted: Boolean,
    cameraRefused: Boolean,
    onRequestCamera: () -> Unit,
    onOpenAppSettings: () -> Unit,
    onRequestLocation: () -> Unit,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by FerryEngine.pairing.collectAsState()
    val method by FerryEngine.pairingMethod.collectAsState()
    val shortCode by FerryEngine.shortCode.collectAsState()
    val scanSent by FerryEngine.scanSent.collectAsState()
    val reachable by FerryEngine.reachable.collectAsState()
    val error by FerryEngine.error.collectAsState()

    val step = pairingStepOf(
        state = state,
        method = method,
        shortCode = shortCode,
        cameraRefused = cameraRefused,
        scanSent = scanSent,
    )

    // Pairing by code needs the phone to be reachable, because the Mac
    // dials it. Scanning does not: this phone dials the Mac. Devices turns
    // the switch on as part of the Pair tap, and the service that does it
    // starts on its own thread, so the code method waits for the engine to
    // report reachable before it asks for pairing.
    LaunchedEffect(method, reachable) {
        if (method == PairingMethod.Code && reachable) {
            FerryEngine.startPairing(PairingMethod.Code)
        }
    }

    val cancel = {
        FerryEngine.cancelPairing()
        onDone()
    }
    val useCode = { FerryEngine.startPairing(PairingMethod.Code) }
    val useScan = {
        if (cameraGranted) {
            FerryEngine.startPairing(PairingMethod.Scan)
        } else {
            // The method has to be recorded before the prompt, not after:
            // pairingStepOf's CameraRefused guard needs method == Scan
            // already set once cameraRefused turns true.
            FerryEngine.setPairingMethod(PairingMethod.Scan)
            onRequestCamera()
        }
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
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = FerryColor.surface(),
                ),
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
        ) {
            when (step) {
                is PairingStep.Choosing -> ChoosingContent(
                    onScan = useScan,
                    onUseCode = useCode,
                    onRequestLocation = onRequestLocation,
                )

                is PairingStep.Scanning -> ScanningContent(
                    onScanned = { FerryEngine.offerScanned(it) },
                    onUseCode = useCode,
                )

                is PairingStep.CameraRefused -> Padded {
                    // The words are the engine's table's, not this view's,
                    // so the phone and the Mac say the same thing about the
                    // same refusal.
                    ErrorBlock(
                        error = threePartError(CAMERA_REFUSED_CODE),
                        onAction = onOpenAppSettings,
                        actionLabel = stringResource(R.string.action_open_settings),
                    )
                    Spacer(Modifier.height(FerrySpace.s4))
                    AlternativeButton(
                        label = stringResource(R.string.pairing_use_code_instead),
                        onClick = useCode,
                    )
                }

                is PairingStep.Scanned -> Padded {
                    val failure = error
                    if (failure != null) {
                        // The engine refused the scan. _scanSent stays
                        // true, so the camera does not reopen and send it
                        // again; this is where the person finds out why,
                        // in the engine's own words, with the other way
                        // in as the way out.
                        ErrorBlock(error = threePartError(failure))
                        Spacer(Modifier.height(FerrySpace.s4))
                        AlternativeButton(
                            label = stringResource(R.string.pairing_use_code_instead),
                            onClick = useCode,
                        )
                    } else if (step.deviceName != null) {
                        // The handshake found the Mac and named it. This
                        // phone asks its own question before it stores the
                        // Mac, the same as the code method's Confirm does,
                        // in the same shape of controls.
                        Text(
                            text = stringResource(R.string.pairing_scanned_named, step.deviceName),
                            style = FerryFont.title(),
                            color = FerryColor.text(),
                        )
                        Spacer(Modifier.height(FerrySpace.s2))
                        Text(
                            text = stringResource(R.string.pairing_scanned_confirm_question),
                            style = FerryFont.body(),
                            color = FerryColor.textSecondary(),
                        )
                        Spacer(Modifier.height(FerrySpace.s3))
                        Row(horizontalArrangement = Arrangement.spacedBy(FerrySpace.s3)) {
                            OutlinedButton(onClick = cancel) {
                                Text(stringResource(R.string.action_cancel))
                            }
                            Button(onClick = { FerryEngine.confirmPairing(true) }) {
                                Text(stringResource(R.string.action_confirm))
                            }
                        }
                    } else {
                        // The handshake with the Mac is still under way:
                        // no name to ask about yet.
                        Text(
                            text = stringResource(R.string.pairing_scanned),
                            style = FerryFont.title(),
                            color = FerryColor.text(),
                        )
                        Spacer(Modifier.height(FerrySpace.s5))
                        TextButton(onClick = cancel) {
                            Text(stringResource(R.string.action_cancel))
                        }
                    }
                }

                is PairingStep.Waiting -> Padded {
                    // Pair turns advertising on and navigates here in the
                    // same tap, before the service has necessarily started
                    // it. While reachable is still false the engine has not
                    // begun pairing, whatever this step's own words say, so
                    // this shows "Starting." instead of "Pairing on." It
                    // shows the engine's own error instead, if starting the
                    // service failed and reachable never turns true.
                    val starting = method == PairingMethod.Code && !reachable
                    val startFailure = error
                    if (starting && startFailure != null) {
                        ErrorBlock(error = threePartError(startFailure))
                    } else {
                        WaitingContent(
                            starting = starting,
                            shortCode = step.shortCode,
                            expiresUnixSecs = step.expiresUnixSecs,
                            onCancel = cancel,
                            onScan = useScan,
                        )
                    }
                }

                is PairingStep.Code -> Padded {
                    PairingCode(
                        code = step.digits,
                        onConfirm = { FerryEngine.confirmPairing(true) },
                        onCancel = {
                            // Rejecting drops the device the code belongs
                            // to. Leaving pairing after that ends the whole
                            // flow, which is what Cancel says it does.
                            FerryEngine.confirmPairing(false)
                            cancel()
                        },
                    )
                    Spacer(Modifier.height(FerrySpace.s3))
                    Countdown(expiresUnixSecs = step.expiresUnixSecs)
                }

                is PairingStep.Confirmed -> Padded {
                    ConfirmedContent()
                    LaunchedEffect(step.deviceId) {
                        // docs-v2/components.md holds the paired icon for
                        // one second before the view closes.
                        delay(CONFIRMED_MILLIS)
                        FerryEngine.cancelPairing()
                        onDone()
                    }
                }

                is PairingStep.Failed -> Padded {
                    ErrorBlock(
                        error = threePartError(step.code, step.detail),
                        onAction = { step.method?.let { FerryEngine.startPairing(it) } },
                    )
                    Spacer(Modifier.height(FerrySpace.s4))
                    // The other way in, named. A scan that failed because
                    // the code expired or was not a Ferry code is often
                    // fastest to escape by typing, and the reverse is true
                    // of a code that timed out.
                    if (step.method == PairingMethod.Scan) {
                        AlternativeButton(
                            label = stringResource(R.string.pairing_use_code_instead),
                            onClick = useCode,
                        )
                    } else {
                        AlternativeButton(
                            label = stringResource(R.string.pairing_scan_instead),
                            onClick = useScan,
                        )
                    }
                }
            }
        }
    }
}

// How long the paired icon stays before Devices comes back.
private const val CONFIRMED_MILLIS = 1_000L

// The one code this screen names directly. Its words, like every other
// error's, come from the generated table.
private const val CAMERA_REFUSED_CODE = "PairingError::CameraRefused"

@Composable
private fun Padded(content: @Composable () -> Unit) {
    Column(modifier = Modifier.padding(FerrySpace.s4)) { content() }
}

// Both ways in, side by side, with the fewer-steps one first.
//
// This is also where location is asked for: only the network name needs
// it, and only pairing needs the name, so this is the first screen worth
// asking from. The line explaining why renders before the prompt fires,
// because onRequestLocation runs in a LaunchedEffect against this
// composable's own entry, not the tap that got here.
@Composable
private fun ChoosingContent(
    onScan: () -> Unit,
    onUseCode: () -> Unit,
    onRequestLocation: () -> Unit,
) {
    LaunchedEffect(Unit) { onRequestLocation() }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(FerrySpace.s4),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = stringResource(R.string.pairing_choose_title),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
        Spacer(Modifier.height(FerrySpace.s2))
        Text(
            text = stringResource(R.string.pairing_location_reason),
            style = FerryFont.body(),
            color = FerryColor.textSecondary(),
        )
        Spacer(Modifier.height(FerrySpace.s5))
        Button(
            onClick = onScan,
            modifier = Modifier
                .fillMaxWidth()
                .height(MIN_TARGET),
            colors = ButtonDefaults.buttonColors(containerColor = FerryColor.accent()),
        ) {
            Text(stringResource(R.string.pairing_scan_the_code))
        }
        Spacer(Modifier.height(FerrySpace.s2))
        AlternativeButton(
            label = stringResource(R.string.pairing_use_code_instead),
            onClick = onUseCode,
        )
    }
}

@Composable
private fun ScanningContent(onScanned: (ByteArray) -> Unit, onUseCode: () -> Unit) {
    Column(modifier = Modifier.fillMaxSize()) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f),
            contentAlignment = Alignment.Center,
        ) {
            QrScanner(onScanned = onScanned, modifier = Modifier.fillMaxSize())
            // The frame is the only thing drawn over the camera, and it is
            // drawn in white because it sits on whatever the camera sees
            // and no token can be relied on to contrast with that.
            Box(
                modifier = Modifier
                    .size(FRAME)
                    .border(2.dp, Color.White, RoundedCornerShape(FerryRadius.large)),
            )
            Text(
                text = stringResource(R.string.pairing_point_the_camera),
                style = FerryFont.body(),
                color = Color.White,
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(FerrySpace.s5)
                    .background(
                        color = Color.Black.copy(alpha = SCRIM_ALPHA),
                        shape = RoundedCornerShape(FerryRadius.medium),
                    )
                    .padding(FerrySpace.s3),
            )
        }
        // Always in reach, never behind a back gesture: a person whose
        // camera cannot read the Mac's screen needs the other way in from
        // here, not from a screen ago.
        AlternativeButton(
            label = stringResource(R.string.pairing_use_code_instead),
            onClick = onUseCode,
            modifier = Modifier.padding(FerrySpace.s4),
        )
    }
}

@Composable
private fun WaitingContent(
    starting: Boolean,
    shortCode: String?,
    expiresUnixSecs: Long,
    onCancel: () -> Unit,
    onScan: () -> Unit,
) {
    Column {
        Text(
            text = stringResource(
                if (starting) R.string.pairing_starting else R.string.pairing_waiting_title,
            ),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
        Spacer(Modifier.height(FerrySpace.s4))
        if (shortCode != null) {
            // The only place this phone's random mDNS name is ever shown. A
            // person in a room with three phones can tell which is theirs,
            // and a wrong pick is safe anyway, because the six digits will
            // not match.
            Text(
                text = stringResource(R.string.pairing_short_code_is),
                style = FerryFont.body(),
                color = FerryColor.textSecondary(),
            )
            Text(
                text = shortCode,
                style = FerryFont.display(),
                color = FerryColor.text(),
            )
        }
        Spacer(Modifier.height(FerrySpace.s3))
        Countdown(expiresUnixSecs = expiresUnixSecs)
        Spacer(Modifier.height(FerrySpace.s5))
        TextButton(onClick = onCancel) {
            Text(stringResource(R.string.action_cancel))
        }
        Spacer(Modifier.height(FerrySpace.s2))
        AlternativeButton(
            label = stringResource(R.string.pairing_scan_instead),
            onClick = onScan,
        )
    }
}

// The two minute timeout, counted. Not "soon", which is an adjective
// standing in for a number.
//
// The engine publishes the deadline on every pairing state, so this counts
// down from that rather than from when the screen opened: a screen reopened
// mid-pairing shows the time that is actually left.
@Composable
private fun Countdown(expiresUnixSecs: Long) {
    if (expiresUnixSecs <= 0L) {
        return
    }
    var now by remember { mutableLongStateOf(System.currentTimeMillis() / 1_000L) }
    LaunchedEffect(expiresUnixSecs) {
        while (true) {
            delay(TICK_MILLIS)
            now = System.currentTimeMillis() / 1_000L
        }
    }
    val remaining = (expiresUnixSecs - now).coerceAtLeast(0L)
    Text(
        text = stringResource(R.string.pairing_stops_in, formatCountdown(remaining)),
        style = FerryFont.caption(),
        color = FerryColor.textSecondary(),
    )
}

@Composable
private fun ConfirmedContent() {
    Column {
        Icon(
            imageVector = ferryIconFor(FerryIcon.paired),
            contentDescription = null,
            tint = FerryColor.accent(),
            modifier = Modifier.size(FerrySpace.s7),
        )
        Spacer(Modifier.height(FerrySpace.s3))
        Text(
            text = stringResource(R.string.pairing_confirmed_label),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
    }
}

// The other way in. One shape for it, used on every step, so it reads as
// the same offer each time rather than as a different control.
@Composable
private fun AlternativeButton(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    TextButton(
        onClick = onClick,
        modifier = modifier
            .fillMaxWidth()
            .height(MIN_TARGET),
        colors = ButtonDefaults.textButtonColors(contentColor = FerryColor.accentText()),
    ) {
        Text(label)
    }
}

// Material's minimum touch target. Nothing a finger lands on is smaller.
private val MIN_TARGET = 48.dp

private val FRAME = 240.dp

private const val SCRIM_ALPHA = 0.6f

private const val TICK_MILLIS = 1_000L
