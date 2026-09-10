package app.ferry.model

import app.ferry.FerryErrors

// Plain data that mirrors what the Rust core will hand the UI once it is
// linked. Nothing here talks to the core yet. FerryApp shows this sample
// data so every screen has something real to render.
//
// A value that is itself a sentence (a paused reason, a three-part error) is
// stored here as plain text, not in strings.xml, because at runtime the
// core generates that text, the same way the generated FerryErrors table
// (Errors.kt, from design/errors.json) does today. strings.xml holds the
// fixed wording around it: labels, titles, button text, and the templates
// that place a number into a sentence.

// One way Ferry reaches a device. Matches docs/ia.md's Transport object.
enum class Transport {
    Usb,
    Wifi,
}

// How a device is reachable right now, for TransportBadge and DeviceRow.
// Matches the state table in docs/components.md.
sealed class ConnectionState {
    data object Idle : ConnectionState()
    data class Moving(val speedMBps: Int) : ConnectionState()
    data object Connecting : ConnectionState()
    data object NotReachable : ConnectionState()
}

// A Mac this phone has paired with. Matches docs/ia.md's Device object.
data class DeviceInfo(
    val id: String,
    val name: String,
    val transport: Transport,
    val connectionState: ConnectionState,
    // Set only when connectionState is NotReachable.
    val lastSeenText: String? = null,
    val pairedOnText: String,
    val keyFingerprint: String,
)

// One file moving in one direction. Matches docs/ia.md's Transfer object.
enum class TransferState {
    Active,
    Paused,
    Done,
    Failed,
}

// The three-part rule from docs/voice.md: what stopped, why, what to do.
// Any part the core does not know is left null, never guessed.
data class ThreePartError(
    val stopped: String,
    val why: String?,
    val todo: String?,
)

data class TransferInfo(
    val id: String,
    val deviceId: String,
    val fileName: String,
    val transport: Transport,
    val state: TransferState,
    // Active
    val filesDone: Int? = null,
    val filesTotal: Int? = null,
    val bytesRemainingText: String? = null,
    val speedMBps: Int? = null,
    // Paused
    val pausedReason: String? = null,
    // Done
    val doneFileCount: Int? = null,
    val doneSizeText: String? = null,
    val doneDurationText: String? = null,
    // Failed
    val error: ThreePartError? = null,
)

// The act of trusting a new Mac, from the phone's side of docs/ia.md's
// first run: the phone turns pairing on and shows its own short code, then
// the six digit code, then confirms.
sealed class PairingState {
    data class Waiting(val shortCode: String) : PairingState()
    data class Code(val code: String) : PairingState()
    data object Confirmed : PairingState()
    data class Failed(val error: ThreePartError) : PairingState()
}

object SampleState {
    // Devices, populated: one reachable over USB and moving bytes, one not
    // reachable and last seen 2 hours ago.
    val devicesPopulated: List<DeviceInfo> = listOf(
        DeviceInfo(
            id = "mac-1",
            name = "MacBook Pro",
            transport = Transport.Usb,
            connectionState = ConnectionState.Moving(speedMBps = 38),
            pairedOnText = "12 August 2026",
            keyFingerprint = "9F3A 7C21 88E0 4B6D 5A19 D302 6E4F 11C8",
        ),
        DeviceInfo(
            id = "mac-2",
            name = "MacBook Air",
            transport = Transport.Wifi,
            connectionState = ConnectionState.NotReachable,
            lastSeenText = "2 hours",
            pairedOnText = "3 July 2026",
            keyFingerprint = "51D8 0E4F A639 2C17 8B0A F764 3D2E 90B5",
        ),
    )
    val devicesEmpty: List<DeviceInfo> = emptyList()

    // Transfers, populated: active, paused by a dropped cable, done, and
    // failed with a three-part error a person can retry.
    val transfersPopulated: List<TransferInfo> = listOf(
        TransferInfo(
            id = "t-active",
            deviceId = "mac-1",
            fileName = "IMG_0512.jpg",
            transport = Transport.Usb,
            state = TransferState.Active,
            filesDone = 3,
            filesTotal = 120,
            bytesRemainingText = "2.1 GB",
            speedMBps = 38,
        ),
        TransferInfo(
            id = "t-paused",
            deviceId = "mac-1",
            fileName = "VID_0299.mp4",
            transport = Transport.Usb,
            state = TransferState.Paused,
            pausedReason = "The cable was disconnected. Reconnect to continue.",
        ),
        TransferInfo(
            id = "t-done",
            deviceId = "mac-1",
            fileName = "Camera roll",
            transport = Transport.Usb,
            state = TransferState.Done,
            doneFileCount = 120,
            doneSizeText = "4.8 GB",
            doneDurationText = "3 min",
        ),
        TransferInfo(
            id = "t-failed",
            deviceId = "mac-2",
            fileName = "IMG_0410.jpg",
            transport = Transport.Wifi,
            state = TransferState.Failed,
            // TransferError::ChunkFailedVerification, from the generated
            // FerryErrors table (Errors.kt), the same table the runtime
            // will use. The !! is deliberate: a missing code here is a bug
            // to catch at once, not to paper over with a guess.
            error = FerryErrors.wordsFor("TransferError::ChunkFailedVerification")!!.let {
                ThreePartError(stopped = it.stopped, why = it.why, todo = it.todo)
            },
        ),
    )
    val transfersEmpty: List<TransferInfo> = emptyList()

    // Pairing, one sample per state, cycled through by PairingScreen's
    // segmented control. The short code is the last four characters of
    // this phone's own mDNS name (docs/ia.md, "What this requires from the
    // protocol").
    val pairingWaiting = PairingState.Waiting(shortCode = "3F9A")
    val pairingCode = PairingState.Code(code = "481 920")
    val pairingConfirmed = PairingState.Confirmed
    val pairingFailed = PairingState.Failed(
        error = ThreePartError(
            stopped = "Pairing stopped.",
            why = "The codes did not match.",
            todo = "Try again.",
        ),
    )
}
